//! 在界面里一键下载本机算力：推理运行时（Ollama）和选中的模型，放进数据目录下的 compute 文件夹，
//! 下完写好用场说明并启动。做的事和 scripts/compute-pack.sh 一样，只是由程序自己来做，并报告进度。
//! 中断后再点一次会接着下：已下完的文件跳过，没下完的从断点续传；每个文件下完都校验。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use commonbot_core::Db;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::compute::{find_pack, Compute};

/// 从哪里下载。可以用环境变量换成镜像地址。
#[derive(Debug, Clone)]
pub struct Sources {
    /// 模型库地址。
    pub registry: String,
    /// 这个平台的运行时压缩包；没有适用的运行时（比如 Windows）时为空，不提供下载。
    pub runtime_url: Option<String>,
}

impl Sources {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let runtime = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "aarch64") => Some("https://ollama.com/download/ollama-linux-arm64.tar.zst"),
            ("linux", "x86_64") => Some("https://ollama.com/download/ollama-linux-amd64.tar.zst"),
            ("macos", _) => Some("https://ollama.com/download/ollama-darwin.tgz"),
            _ => None,
        };
        Self {
            registry: env("COMMONBOT_COMPUTE_REGISTRY").unwrap_or_else(|| "https://registry.ollama.ai".into()),
            runtime_url: env("COMMONBOT_COMPUTE_RUNTIME_URL").or(runtime.map(String::from)),
        }
    }
}

/// 可选的模型：用途、名称、大约多大（GB）。按 48 GB 内存的机器选的（见 D35）。
const CATALOG: [(&str, &str, f64); 4] = [
    ("chat", "qwen3:14b", 9.3),
    ("extra", "qwen3:30b", 18.6),
    ("embed", "bge-m3", 1.2),
    ("vision", "qwen2.5vl:7b", 6.0),
];

/// 实际提供的模型。可以用环境变量 COMMONBOT_COMPUTE_MODELS 换成别的，
/// 写法：`chat=qwen3:8b,embed=bge-m3`（用途只能是 chat / extra / embed / vision；大小未知，界面不显示）。
fn models() -> Vec<(String, String, f64)> {
    let custom: Vec<_> = std::env::var("COMMONBOT_COMPUTE_MODELS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(role, model)| CATALOG.iter().any(|(r, ..)| r == role) && !model.is_empty())
        .map(|(role, model)| (role.to_string(), model.to_string(), 0.0))
        .collect();
    if custom.is_empty() {
        CATALOG.iter().map(|(r, m, gb)| (r.to_string(), m.to_string(), *gb)).collect()
    } else {
        custom
    }
}

pub fn catalog() -> Value {
    models().iter().map(|(role, model, gb)| json!({ "role": role, "model": model, "gb": gb })).collect()
}

/// 下载进度，界面据此显示。
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// idle（没在下）/ running / failed
    pub status: &'static str,
    /// 正在做哪一步：prepare / runtime / models / starting
    pub step: &'static str,
    /// 正在下载的东西（模型名）。
    pub item: String,
    pub done: u64,
    pub total: u64,
    /// 失败原因的代码，界面按它显示对应语言的说明。
    pub error: Option<&'static str>,
    pub detail: Option<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Self { status: "idle", step: "", item: String::new(), done: 0, total: 0, error: None, detail: None }
    }
}

#[derive(Debug)]
pub struct Fail {
    pub code: &'static str,
    pub detail: String,
}

fn fail(code: &'static str, detail: impl std::fmt::Display) -> Fail {
    Fail { code, detail: detail.to_string() }
}

/// 一个要下载的文件。
struct Blob {
    url: String,
    hex: String,
    size: u64,
    /// 属于哪个模型，显示进度用。
    model: String,
}

impl Compute {
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn update(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().unwrap_or_else(|p| p.into_inner()));
    }

    pub fn cancel_download(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn cancelled(&self) -> Result<(), Fail> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(fail("cancelled", ""))
        } else {
            Ok(())
        }
    }

    /// 开始下载。马上返回，下载在后台进行；进度用 `progress()` 看。
    pub async fn start_download(self: &Arc<Self>, db: Db, roles: &[String]) -> Result<(), Fail> {
        if self.pack().is_some() {
            return Err(fail("compute_exists", "a compute pack is already installed"));
        }
        if self.sources.runtime_url.is_none() {
            return Err(fail("compute_unsupported", "no runtime for this platform"));
        }
        let mut info = crate::compute::PackInfo {
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            ..Default::default()
        };
        for (role, model, _) in models().iter().filter(|(role, ..)| roles.iter().any(|r| r == role)) {
            match role.as_str() {
                "chat" => info.chat = model.to_string(),
                "extra" => info.extra.push(model.to_string()),
                "embed" => info.embed = model.to_string(),
                _ => info.vision = model.to_string(),
            }
        }
        // 只选了备选的对话模型：它就是日常用的那个
        if info.chat.is_empty() && !info.extra.is_empty() {
            info.chat = info.extra.remove(0);
        }
        if info.chat.is_empty() && info.embed.is_empty() && info.vision.is_empty() {
            return Err(fail("compute_nothing", "choose at least one model"));
        }
        // 端口上已经有推理服务：下来的运行时启动不了，模型也到不了那个服务里
        if self.answering().await {
            return Err(fail("compute_port_busy", "an inference service is already running on this machine"));
        }
        {
            let mut p = self.progress.lock().unwrap_or_else(|p| p.into_inner());
            if p.status == "running" {
                return Err(fail("compute_busy", "a download is already running"));
            }
            *p = Progress { status: "running", step: "prepare", ..Default::default() };
        }
        self.cancel.store(false, Ordering::SeqCst);
        let me = self.clone();
        tokio::spawn(async move {
            match me.download(&info).await {
                Ok(pack) => {
                    me.update(|p| p.step = "starting");
                    me.set_pack(pack);
                    me.spawn(db);
                    me.update(|p| *p = Progress::default());
                }
                Err(f) if f.code == "cancelled" => me.update(|p| *p = Progress::default()),
                Err(f) => me.update(|p| {
                    p.status = "failed";
                    p.error = Some(f.code);
                    p.detail = Some(f.detail);
                }),
            }
        });
        Ok(())
    }

    async fn download(&self, info: &crate::compute::PackInfo) -> Result<crate::compute::Pack, Fail> {
        let io = |e: std::io::Error| fail("compute_disk", e);
        let dest = self.data_dir.join("compute");
        let blobs_dir = dest.join("models/blobs");
        tokio::fs::create_dir_all(&blobs_dir).await.map_err(io)?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| fail("compute_network", e))?;
        let registry = self.sources.registry.trim_end_matches('/');
        let runtime_url = self.sources.runtime_url.clone().unwrap_or_default();

        // 1. 取每个模型的清单，算出要下载哪些文件、一共多大
        let models: Vec<&String> = [&info.chat]
            .into_iter()
            .chain(&info.extra)
            .chain([&info.embed, &info.vision])
            .filter(|m| !m.is_empty())
            .collect();
        let mut manifests = Vec::new();
        let mut blobs: BTreeMap<String, Blob> = BTreeMap::new();
        for model in models {
            self.cancelled()?;
            let (name, tag) = model.split_once(':').unwrap_or((model, "latest"));
            let repo = if name.contains('/') { name.to_string() } else { format!("library/{name}") };
            let resp = http
                .get(format!("{registry}/v2/{repo}/manifests/{tag}"))
                .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
                .send()
                .await
                .map_err(|e| fail("compute_network", e))?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(fail("compute_not_found", model));
            }
            let text = resp
                .error_for_status()
                .map_err(|e| fail("compute_network", e))?
                .text()
                .await
                .map_err(|e| fail("compute_network", e))?;
            let manifest: Value =
                serde_json::from_str(&text).map_err(|e| fail("compute_network", format!("{model}: {e}")))?;
            for part in manifest["layers"].as_array().into_iter().flatten().chain([&manifest["config"]]) {
                let hex = part["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).unwrap_or_default();
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(fail("compute_network", format!("{model}: unexpected manifest")));
                }
                blobs.entry(hex.to_string()).or_insert_with(|| Blob {
                    url: format!("{registry}/v2/{repo}/blobs/sha256:{hex}"),
                    hex: hex.to_string(),
                    size: part["size"].as_u64().unwrap_or(0),
                    model: model.clone(),
                });
            }
            manifests.push((dest.join("models/manifests/registry.ollama.ai").join(&repo).join(tag), text));
        }
        let runtime_done = dest.join("runtime/.done").is_file();
        let runtime_size = if runtime_done {
            0
        } else {
            http.head(&runtime_url)
                .send()
                .await
                .ok()
                .and_then(|r| r.error_for_status().ok())
                .and_then(|r| r.content_length())
                .unwrap_or(0)
        };
        let present =
            |b: &Blob| std::fs::metadata(blobs_dir.join(format!("sha256-{}", b.hex))).is_ok_and(|m| m.len() == b.size);
        let part_len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let total = runtime_size + blobs.values().map(|b| b.size).sum::<u64>();
        let have = blobs
            .values()
            .map(|b| {
                if present(b) {
                    b.size
                } else {
                    part_len(&blobs_dir.join(format!("sha256-{}.part", b.hex))).min(b.size)
                }
            })
            .sum::<u64>()
            + part_len(&dest.join("runtime.download")).min(runtime_size);
        self.update(|p| {
            p.total = total;
            p.done = have;
        });

        // 2. 磁盘够不够：还要下的部分，加上运行时解压要占的地方
        let need = (total - have) + runtime_size * 3;
        let need = need + need / 20;
        if let Ok(free) = fs4::available_space(&dest) {
            if free < need {
                let gb = |n: u64| format!("{:.1}", n as f64 / (1u64 << 30) as f64);
                return Err(fail("compute_disk", format!("need {} GB free, have {} GB", gb(need), gb(free))));
            }
        }

        // 3. 运行时
        if !runtime_done {
            self.update(|p| p.step = "runtime");
            let archive = dest.join("runtime.download");
            self.fetch(&http, &runtime_url, &archive).await?;
            let (src, out, tmp, zst) =
                (archive.clone(), dest.join("runtime"), dest.join("runtime.unpack"), runtime_url.ends_with(".zst"));
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                let _ = std::fs::remove_dir_all(&tmp);
                let _ = std::fs::remove_dir_all(&out);
                std::fs::create_dir_all(&tmp)?;
                let file = std::fs::File::open(&src)?;
                let reader: Box<dyn Read> = if zst {
                    Box::new(zstd::Decoder::new(file)?)
                } else {
                    Box::new(flate2::read::GzDecoder::new(file))
                };
                tar::Archive::new(reader).unpack(&tmp)?;
                std::fs::write(tmp.join(".done"), "")?;
                std::fs::rename(&tmp, &out)?;
                std::fs::remove_file(&src)
            })
            .await
            .map_err(|e| fail("compute_disk", e))?
            .map_err(|e| fail("compute_runtime", e))?;
        }

        // 4. 模型文件
        self.update(|p| p.step = "models");
        for blob in blobs.values().filter(|b| !present(b)) {
            self.update(|p| p.item = blob.model.clone());
            let file = blobs_dir.join(format!("sha256-{}", blob.hex));
            let part = blobs_dir.join(format!("sha256-{}.part", blob.hex));
            self.fetch(&http, &blob.url, &part).await?;
            let (path, want) = (part.clone(), blob.hex.clone());
            let ok = tokio::task::spawn_blocking(move || sha256_file(&path).map(|got| got == want))
                .await
                .map_err(|e| fail("compute_disk", e))?
                .map_err(io)?;
            if !ok {
                // 内容不对：删掉，下次从头下这个文件
                let had = part_len(&part);
                let _ = tokio::fs::remove_file(&part).await;
                self.update(|p| p.done = p.done.saturating_sub(had));
                return Err(fail("compute_checksum", &blob.model));
            }
            tokio::fs::rename(&part, &file).await.map_err(io)?;
        }

        // 5. 清单和用场说明：全部文件到齐之后才写，半截的包不会被当成可用的
        for (path, text) in manifests {
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir).await.map_err(io)?;
            }
            tokio::fs::write(&path, text).await.map_err(io)?;
        }
        let built =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut pack = serde_json::to_value(info).unwrap_or_default();
        pack["format"] = json!(1);
        pack["built"] = json!(format!("downloaded-{built}"));
        tokio::fs::write(dest.join("pack.json"), pack.to_string()).await.map_err(io)?;
        find_pack(std::slice::from_ref(&self.data_dir))
            .ok_or_else(|| fail("compute_runtime", "the downloaded runtime has no ollama program"))
    }

    /// 把一个地址下载到文件，断了就从断点接着下，最多连着失败几次。
    async fn fetch(&self, http: &reqwest::Client, url: &str, path: &PathBuf) -> Result<(), Fail> {
        let io = |e: std::io::Error| fail("compute_disk", e);
        let mut failures = 0;
        loop {
            self.cancelled()?;
            let have = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let mut req = http.get(url);
            if have > 0 {
                req = req.header("Range", format!("bytes={have}-"));
            }
            let attempt: Result<bool, String> = async {
                let resp = req.send().await.map_err(|e| e.to_string())?;
                let status = resp.status();
                // 已经整个下完了（断点正好在末尾）
                if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                    return Ok(true);
                }
                if !status.is_success() {
                    return Err(format!("{url}: {status}"));
                }
                let resume = status == reqwest::StatusCode::PARTIAL_CONTENT;
                if !resume && have > 0 {
                    // 服务器不支持续传：从头来
                    self.update(|p| p.done = p.done.saturating_sub(have));
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(resume)
                    .truncate(!resume)
                    .open(path)
                    .await
                    .map_err(|e| format!("disk: {e}"))?;
                let mut stream = resp.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    if self.cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let chunk = chunk.map_err(|e| e.to_string())?;
                    file.write_all(&chunk).await.map_err(|e| format!("disk: {e}"))?;
                    self.update(|p| p.done += chunk.len() as u64);
                }
                file.flush().await.map_err(|e| format!("disk: {e}"))?;
                Ok(true)
            }
            .await;
            match attempt {
                Ok(_) => return self.cancelled(),
                Err(e) if e.starts_with("disk: ") => return Err(io(std::io::Error::other(e))),
                Err(e) => {
                    // 有进展就不算连续失败
                    let now = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    failures = if now > have { 0 } else { failures + 1 };
                    if failures >= 5 {
                        return Err(fail("compute_network", e));
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
