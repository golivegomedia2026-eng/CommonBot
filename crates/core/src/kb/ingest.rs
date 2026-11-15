//! 入库队列：一份一份地处理上传的文档——读文字（必要时识别）、切片段、向量化。
//! 一次只处理一份，避免和对话抢模型。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use super::index::VectorIndex;
use super::parse::{self, Kind, Locator, Section};
use super::{chunk, Document, KbModels};
use crate::llm::{Endpoint, LlmClient};
use crate::store::Provider;
use crate::{Db, Error, Result};

/// 一页里的文字少于这么多个字符，就当作扫描页交给看图模型。
const SCANNED_PAGE_CHARS: usize = 16;
/// 每次向量化多少个片段。
const EMBED_BATCH: usize = 16;

pub struct Ingestor {
    pub db: Db,
    pub llm: LlmClient,
    pub index: Arc<VectorIndex>,
    /// 原文件存放处：`<files_dir>/<文档编号>/original.<扩展名>`。
    pub files_dir: PathBuf,
    /// PDF 解析库的位置；没有时 PDF 无法入库。
    pub pdfium: Option<PathBuf>,
    wake: Notify,
}

fn endpoint(p: &Provider) -> Endpoint {
    Endpoint { base_url: p.base_url.clone(), api_key: p.api_key.clone() }
}

/// 处理被暂停或文档被删除，不算失败。
struct Stopped;

enum Outcome {
    Done(Option<String>),
    Stopped,
}

impl From<Stopped> for Outcome {
    fn from(_: Stopped) -> Self {
        Outcome::Stopped
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    // 解析库遇到损坏的文件可能崩溃；把崩溃变成「文件损坏」而不是让整个服务退出
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::Doc("doc_damaged", e.to_string()))?
}

impl Ingestor {
    pub fn new(
        db: Db,
        llm: LlmClient,
        index: Arc<VectorIndex>,
        files_dir: PathBuf,
        pdfium: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self { db, llm, index, files_dir, pdfium, wake: Notify::new() })
    }

    pub fn file_path(&self, doc_id: &str, ext: &str) -> PathBuf {
        self.files_dir.join(doc_id).join(format!("original.{ext}"))
    }

    /// 有新文档排队时调用。
    pub fn kick(&self) {
        self.wake.notify_one();
    }

    /// 启动后台处理。返回后队列即开始工作。
    pub fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.db.run(|c| super::requeue_interrupted(c).map(|_| ())).await {
                eprintln!("knowledge queue: {e}");
            }
            loop {
                match me.db.run(super::claim_next).await {
                    Ok(Some(doc)) => me.handle(doc).await,
                    Ok(None) => me.wake.notified().await,
                    Err(e) => {
                        eprintln!("knowledge queue: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    async fn handle(&self, doc: Document) {
        let id = doc.id.clone();
        let result = self.process(&doc).await;
        let saved = match result {
            Ok(Outcome::Done(model)) => self.db.run(move |c| super::finish(c, &id, model.as_deref())).await,
            Ok(Outcome::Stopped) => Ok(()),
            Err(e) => {
                let (code, detail) = (e.code(), e.to_string());
                self.db.run(move |c| super::fail(c, &id, code, &detail)).await
            }
        };
        if let Err(e) = saved {
            eprintln!("knowledge queue: could not record the result for {}: {e}", doc.name);
        }
    }

    async fn step(
        &self,
        doc_id: &str,
        status: &'static str,
        done: usize,
        total: usize,
    ) -> std::result::Result<(), Outcome> {
        let id = doc_id.to_string();
        match self.db.run(move |c| super::progress(c, &id, status, done as i64, total as i64)).await {
            Ok(true) => Ok(()),
            _ => Err(Outcome::Stopped),
        }
    }

    async fn process(&self, doc: &Document) -> Result<Outcome> {
        let models = self.db.run(|c| super::models(c)).await?;
        if doc.resume_from != "embed" {
            match self.extract(doc, &models).await? {
                Ok(()) => {}
                Err(stopped) => return Ok(stopped),
            }
        }
        match self.embed(doc, &models).await? {
            Ok(model) => Ok(Outcome::Done(model)),
            Err(stopped) => Ok(stopped),
        }
    }

    /// 读出文字并切成片段。
    async fn extract(&self, doc: &Document, models: &KbModels) -> Result<std::result::Result<(), Outcome>> {
        let kind = parse::kind_of(&doc.ext).ok_or_else(|| Error::Doc("doc_unsupported", doc.ext.clone()))?;
        let path = self.file_path(&doc.id, &doc.ext);
        if !path.exists() {
            return Err(Error::Doc("doc_missing", String::new()));
        }
        let (mut pages, mut ocr_pages, mut ocr_skipped) = (None, 0i64, 0i64);

        let sections: Vec<Section> = match kind {
            Kind::Pdf => {
                let lib = self.pdfium.clone().ok_or_else(|| Error::Doc("pdf_engine_missing", String::new()))?;
                let (l, p) = (lib.clone(), path.clone());
                let mut texts = blocking(move || parse::pdf_pages(&l, &p)).await?;
                pages = Some(texts.len() as i64);
                let scanned: Vec<usize> = texts
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.trim().chars().count() < SCANNED_PAGE_CHARS)
                    .map(|(i, _)| i)
                    .collect();
                match (&models.ocr, scanned.is_empty()) {
                    (_, true) => {}
                    (None, false) if scanned.len() == texts.len() => {
                        return Err(Error::Doc("ocr_not_configured", String::new()))
                    }
                    (None, false) => ocr_skipped = scanned.len() as i64,
                    (Some((provider, model)), false) => {
                        for (n, &i) in scanned.iter().enumerate() {
                            if let Err(s) = self.step(&doc.id, "ocr", n, scanned.len()).await {
                                return Ok(Err(s));
                            }
                            let (l, p) = (lib.clone(), path.clone());
                            let png = blocking(move || parse::pdf_render_page(&l, &p, i)).await?;
                            texts[i] = self.llm.ocr_image(&endpoint(provider), model, &png).await?;
                            ocr_pages += 1;
                        }
                    }
                }
                texts
                    .into_iter()
                    .enumerate()
                    .map(|(i, text)| Section { locator: Locator::Page(i as u32 + 1), text })
                    .collect()
            }
            Kind::Image => {
                let (provider, model) =
                    models.ocr.as_ref().ok_or_else(|| Error::Doc("ocr_not_configured", String::new()))?;
                if let Err(s) = self.step(&doc.id, "ocr", 0, 1).await {
                    return Ok(Err(s));
                }
                let p = path.clone();
                let png = blocking(move || parse::image_for_ocr(&p)).await?;
                ocr_pages = 1;
                vec![Section {
                    locator: Locator::None,
                    text: self.llm.ocr_image(&endpoint(provider), model, &png).await?,
                }]
            }
            other => blocking(move || parse::parse(&path, other)).await?,
        };

        let chunks = chunk::chunk(&sections);
        if chunks.is_empty() {
            return Err(Error::Doc("doc_empty", String::new()));
        }
        let id = doc.id.clone();
        self.db.run(move |c| super::replace_chunks(c, &id, &chunks, pages, ocr_pages, ocr_skipped)).await?;
        Ok(Ok(()))
    }

    /// 给片段算向量。没配置向量化模型时跳过，文档仍可按关键词检索。
    async fn embed(&self, doc: &Document, models: &KbModels) -> Result<std::result::Result<Option<String>, Outcome>> {
        let Some((provider, model)) = &models.embed else { return Ok(Ok(None)) };
        let id = doc.id.clone();
        let pending = self.db.run(move |c| super::chunks_to_embed(c, &id, true)).await?;
        let total = pending.len();
        let mut all: Vec<(i64, Vec<f32>)> = Vec::with_capacity(total);
        for (n, batch) in pending.chunks(EMBED_BATCH).enumerate() {
            if let Err(s) = self.step(&doc.id, "embedding", n * EMBED_BATCH, total).await {
                return Ok(Err(s));
            }
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vectors = self.llm.embed(&endpoint(provider), model, &texts).await?;
            let pairs: Vec<(i64, Vec<f32>)> = batch.iter().map(|(id, _)| *id).zip(vectors).collect();
            let save = pairs.clone();
            self.db.run(move |c| super::set_vectors(c, &save)).await?;
            all.extend(pairs);
        }
        if let Err(s) = self.step(&doc.id, "embedding", total, total).await {
            return Ok(Err(s));
        }
        self.index.append(model, &doc.id, &all);
        Ok(Ok(Some(model.clone())))
    }

    /// 删除文档的原文件。
    pub async fn remove_files(&self, doc_ids: Vec<String>) {
        let dir = self.files_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for id in doc_ids {
                // 编号由程序生成，只含字母数字；仍然检查一次，绝不删到目录之外
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()) {
                    let _ = std::fs::remove_dir_all(dir.join(&id));
                }
            }
        })
        .await;
    }
}

/// 找到 PDF 解析库：先看环境变量，再看程序旁边，最后是开发时的 vendor 目录。
pub fn find_pdfium(extra: &[&Path]) -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("COMMONBOT_PDFIUM") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.extend([
                dir.join(name),
                dir.join("lib").join(name),
                dir.join("../lib/commonbot").join(name),
                dir.join("../Resources").join(name),
            ]);
        }
    }
    for dir in extra {
        candidates.push(dir.join(name));
    }
    candidates.into_iter().find(|p| p.is_file())
}
