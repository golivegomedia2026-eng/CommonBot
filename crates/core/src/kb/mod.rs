//! 知识库：文件夹、文档、片段的存取，以及关键词检索。

pub mod chunk;
pub mod index;
pub mod ingest;
pub mod parse;
pub mod search;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::store::{self, Provider};
use crate::{new_id, now_ms, Error, Result};

pub use chunk::Chunk;
pub use parse::Locator;

// ---------- 文件夹 ----------

#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub created_at: i64,
    /// 直接放在这个文件夹里的文档数（不含子文件夹）。
    pub documents: i64,
}

fn folder_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 60 || n.contains('/') {
        return Err(Error::Invalid("name".into()));
    }
    Ok(n.to_string())
}

fn name_taken(c: &Connection, parent: Option<&str>, name: &str, except: Option<&str>) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE parent_id IS ?1 AND name = ?2 AND id IS NOT ?3)",
        params![parent, name, except],
        |r| r.get(0),
    )?)
}

pub fn list_folders(c: &Connection) -> Result<Vec<Folder>> {
    let mut stmt = c.prepare(
        "SELECT f.*, (SELECT count(*) FROM documents d WHERE d.folder_id = f.id) AS documents
         FROM folders f ORDER BY f.name COLLATE NOCASE, f.created_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Folder {
            id: r.get("id")?,
            parent_id: r.get("parent_id")?,
            name: r.get("name")?,
            created_at: r.get("created_at")?,
            documents: r.get("documents")?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn folder_exists(c: &Connection, id: &str) -> Result<()> {
    let found: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)", [id], |r| r.get(0))?;
    if found {
        Ok(())
    } else {
        Err(Error::NotFound("folder"))
    }
}

pub fn create_folder(c: &Connection, name: &str, parent_id: Option<&str>) -> Result<String> {
    let name = folder_name(name)?;
    if let Some(p) = parent_id {
        folder_exists(c, p)?;
    }
    if name_taken(c, parent_id, &name, None)? {
        return Err(Error::Invalid("name_taken".into()));
    }
    let id = new_id();
    c.execute(
        "INSERT INTO folders(id, parent_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, parent_id, name, now_ms()],
    )?;
    Ok(id)
}

/// 这些文件夹及其所有子文件夹。
pub fn subtree(c: &Connection, folder_ids: &[String]) -> Result<Vec<String>> {
    let mut stmt = c.prepare(
        "WITH RECURSIVE sub(id) AS (
            SELECT id FROM folders WHERE id IN (SELECT value FROM json_each(?1))
            UNION SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id)
         SELECT id FROM sub",
    )?;
    let rows = stmt.query_map([serde_json::to_string(folder_ids).unwrap_or_else(|_| "[]".into())], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 改名和/或移动。`parent` 为 `Some(None)` 表示移到最上层。
pub fn update_folder(c: &Connection, id: &str, name: Option<&str>, parent: Option<Option<&str>>) -> Result<()> {
    let (cur_name, cur_parent): (String, Option<String>) = c
        .query_row("SELECT name, parent_id FROM folders WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or(Error::NotFound("folder"))?;
    let name = match name {
        Some(n) => folder_name(n)?,
        None => cur_name,
    };
    let parent: Option<String> = match parent {
        Some(p) => p.map(str::to_string),
        None => cur_parent,
    };
    if let Some(p) = &parent {
        folder_exists(c, p)?;
        // 不能移到自己或自己的子文件夹里
        if subtree(c, &[id.to_string()])?.contains(p) {
            return Err(Error::Invalid("parent".into()));
        }
    }
    if name_taken(c, parent.as_deref(), &name, Some(id))? {
        return Err(Error::Invalid("name_taken".into()));
    }
    c.execute("UPDATE folders SET name = ?2, parent_id = ?3 WHERE id = ?1", params![id, name, parent])?;
    Ok(())
}

/// 删除文件夹及其下的一切。返回被删文档的编号，调用方据此删除磁盘上的原文件。
pub fn delete_folder(c: &mut Connection, id: &str) -> Result<Vec<String>> {
    folder_exists(c, id)?;
    let folders = subtree(c, &[id.to_string()])?;
    let tx = c.transaction()?;
    let docs: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM documents WHERE folder_id IN (SELECT value FROM json_each(?1))")?;
        let rows = stmt.query_map([serde_json::to_string(&folders).unwrap_or_default()], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    tx.execute("DELETE FROM folders WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(docs)
}

/// 文件夹的完整路径，如「招标文件 / 2026 政务云二期」。
pub fn folder_path(c: &Connection, id: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut cur = Some(id.to_string());
    while let Some(fid) = cur {
        let row: Option<(String, Option<String>)> = c
            .query_row("SELECT name, parent_id FROM folders WHERE id = ?1", [&fid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match row {
            Some((name, parent)) if parts.len() < 32 => {
                parts.push(name);
                cur = parent;
            }
            _ => break,
        }
    }
    parts.reverse();
    Ok(parts.join(" / "))
}

// ---------- 文档 ----------

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub id: String,
    pub folder_id: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub status: String,
    #[serde(skip_serializing)]
    pub resume_from: String,
    pub done: i64,
    pub total: i64,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub pages: Option<i64>,
    pub chunks: i64,
    pub ocr_pages: i64,
    pub ocr_skipped: i64,
    pub embed_model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn doc_from_row(r: &rusqlite::Row) -> rusqlite::Result<Document> {
    Ok(Document {
        id: r.get("id")?,
        folder_id: r.get("folder_id")?,
        name: r.get("name")?,
        ext: r.get("ext")?,
        size: r.get("size")?,
        status: r.get("status")?,
        resume_from: r.get("resume_from")?,
        done: r.get("done")?,
        total: r.get("total")?,
        error_code: r.get("error_code")?,
        error_detail: r.get("error_detail")?,
        pages: r.get("pages")?,
        chunks: r.get("chunks")?,
        ocr_pages: r.get("ocr_pages")?,
        ocr_skipped: r.get("ocr_skipped")?,
        embed_model: r.get("embed_model")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

/// 文件名里的扩展名（小写）。
pub fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// 登记一份刚上传的文档，排队等待处理。
pub fn add_document(c: &Connection, id: &str, folder_id: &str, name: &str, size: i64) -> Result<Document> {
    folder_exists(c, folder_id)?;
    let name: String = name.trim().chars().take(200).collect();
    let ext = ext_of(&name);
    if name.is_empty() {
        return Err(Error::Invalid("name".into()));
    }
    if parse::kind_of(&ext).is_none() {
        return Err(Error::Doc("doc_unsupported", ext));
    }
    let now = now_ms();
    c.execute(
        "INSERT INTO documents(id, folder_id, name, ext, size, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, folder_id, name, ext, size, now],
    )?;
    get_document(c, id)
}

pub fn get_document(c: &Connection, id: &str) -> Result<Document> {
    c.query_row("SELECT * FROM documents WHERE id = ?1", [id], doc_from_row)
        .optional()?
        .ok_or(Error::NotFound("document"))
}

pub fn list_documents(c: &Connection, folder_id: &str, recursive: bool) -> Result<Vec<Document>> {
    folder_exists(c, folder_id)?;
    let folders = if recursive { subtree(c, &[folder_id.to_string()])? } else { vec![folder_id.to_string()] };
    let mut stmt = c.prepare(
        "SELECT * FROM documents WHERE folder_id IN (SELECT value FROM json_each(?1)) ORDER BY created_at DESC, id LIMIT 2000",
    )?;
    let rows = stmt.query_map([serde_json::to_string(&folders).unwrap_or_default()], doc_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn delete_document(c: &Connection, id: &str) -> Result<()> {
    match c.execute("DELETE FROM documents WHERE id = ?1", [id])? {
        0 => Err(Error::NotFound("document")),
        _ => Ok(()),
    }
}

pub fn move_document(c: &Connection, id: &str, folder_id: &str) -> Result<()> {
    folder_exists(c, folder_id)?;
    match c.execute(
        "UPDATE documents SET folder_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, folder_id, now_ms()],
    )? {
        0 => Err(Error::NotFound("document")),
        _ => Ok(()),
    }
}

/// 重新排队：`from_start` 为 false 时保留已有片段，只重做向量化。
pub fn requeue(c: &Connection, id: &str, from_start: bool) -> Result<()> {
    let n = c.execute(
        "UPDATE documents SET status = 'queued', resume_from = ?2, done = 0, total = 0, error_code = NULL, error_detail = NULL, updated_at = ?3 WHERE id = ?1",
        params![id, if from_start { "parse" } else { "embed" }, now_ms()],
    )?;
    if n == 0 {
        return Err(Error::NotFound("document"));
    }
    Ok(())
}

/// 暂停一份还没处理完的文档。
pub fn pause(c: &Connection, id: &str) -> Result<()> {
    get_document(c, id)?;
    c.execute(
        "UPDATE documents SET status = 'paused', updated_at = ?2 WHERE id = ?1 AND status IN ('queued', 'parsing', 'ocr', 'embedding')",
        params![id, now_ms()],
    )?;
    Ok(())
}

pub fn resume(c: &Connection, id: &str) -> Result<()> {
    get_document(c, id)?;
    c.execute(
        "UPDATE documents SET status = 'queued', updated_at = ?2 WHERE id = ?1 AND status = 'paused'",
        params![id, now_ms()],
    )?;
    Ok(())
}

/// 取出下一份排队的文档并标记为处理中。
pub fn claim_next(c: &mut Connection) -> Result<Option<Document>> {
    let tx = c.transaction()?;
    let doc = tx
        .query_row("SELECT * FROM documents WHERE status = 'queued' ORDER BY created_at, id LIMIT 1", [], doc_from_row)
        .optional()?;
    if let Some(d) = &doc {
        tx.execute(
            "UPDATE documents SET status = 'parsing', done = 0, total = 0, updated_at = ?2 WHERE id = ?1",
            params![d.id, now_ms()],
        )?;
    }
    tx.commit()?;
    Ok(doc)
}

/// 上次运行时处理到一半的文档，重新排队。
pub fn requeue_interrupted(c: &Connection) -> Result<usize> {
    Ok(c.execute("UPDATE documents SET status = 'queued' WHERE status IN ('parsing', 'ocr', 'embedding')", [])?)
}

/// 更新处理阶段和进度。文档已被删除或暂停时返回 false，调用方应停止处理。
pub fn progress(c: &Connection, id: &str, status: &str, done: i64, total: i64) -> Result<bool> {
    let n = c.execute(
        "UPDATE documents SET status = ?2, done = ?3, total = ?4, updated_at = ?5 WHERE id = ?1 AND status IN ('parsing', 'ocr', 'embedding')",
        params![id, status, done, total, now_ms()],
    )?;
    Ok(n > 0)
}

pub fn fail(c: &Connection, id: &str, code: &str, detail: &str) -> Result<()> {
    let detail: String = detail.chars().take(500).collect();
    c.execute(
        "UPDATE documents SET status = 'failed', error_code = ?2, error_detail = ?3, updated_at = ?4 WHERE id = ?1 AND status IN ('parsing', 'ocr', 'embedding')",
        params![id, code, detail, now_ms()],
    )?;
    Ok(())
}

pub fn finish(c: &Connection, id: &str, embed_model: Option<&str>) -> Result<()> {
    c.execute(
        "UPDATE documents SET status = 'ready', resume_from = 'parse', done = 0, total = 0, error_code = NULL, error_detail = NULL, embed_model = ?2, updated_at = ?3
         WHERE id = ?1 AND status IN ('parsing', 'ocr', 'embedding')",
        params![id, embed_model, now_ms()],
    )?;
    Ok(())
}

// ---------- 片段 ----------

/// 用新切出的片段替换文档原有的片段，并更新全文索引和统计。
pub fn replace_chunks(
    c: &mut Connection,
    doc_id: &str,
    chunks: &[Chunk],
    pages: Option<i64>,
    ocr_pages: i64,
    ocr_skipped: i64,
) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute("DELETE FROM chunks WHERE document_id = ?1", [doc_id])?;
    {
        let mut ins = tx.prepare("INSERT INTO chunks(document_id, seq, locator, text) VALUES (?1, ?2, ?3, ?4)")?;
        let mut fts = tx.prepare("INSERT INTO chunks_fts(rowid, tokens) VALUES (?1, ?2)")?;
        for (i, ch) in chunks.iter().enumerate() {
            let locator = serde_json::to_string(&ch.locator).unwrap_or_else(|_| r#"{"kind":"none"}"#.into());
            ins.execute(params![doc_id, i as i64, locator, ch.text])?;
            fts.execute(params![tx.last_insert_rowid(), chunk::tokens(&ch.text).join(" ")])?;
        }
    }
    tx.execute(
        "UPDATE documents SET pages = ?2, chunks = ?3, ocr_pages = ?4, ocr_skipped = ?5 WHERE id = ?1",
        params![doc_id, pages, chunks.len() as i64, ocr_pages, ocr_skipped],
    )?;
    tx.commit()?;
    Ok(())
}

/// 还没有向量的片段（编号、文字）。
pub fn chunks_to_embed(c: &Connection, doc_id: &str, all: bool) -> Result<Vec<(i64, String)>> {
    let sql = if all {
        "SELECT id, text FROM chunks WHERE document_id = ?1 ORDER BY seq"
    } else {
        "SELECT id, text FROM chunks WHERE document_id = ?1 AND vector IS NULL ORDER BY seq"
    };
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map([doc_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_vector(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect()
}

pub fn set_vectors(c: &mut Connection, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
    let tx = c.transaction()?;
    {
        let mut stmt = tx.prepare("UPDATE chunks SET vector = ?2 WHERE id = ?1")?;
        for (id, v) in vectors {
            stmt.execute(params![id, vector_to_blob(v)])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkView {
    pub seq: i64,
    pub locator: Locator,
    pub text: String,
}

pub fn list_chunks(c: &Connection, doc_id: &str, offset: i64, limit: i64) -> Result<Vec<ChunkView>> {
    get_document(c, doc_id)?;
    let mut stmt =
        c.prepare("SELECT seq, locator, text FROM chunks WHERE document_id = ?1 ORDER BY seq LIMIT ?3 OFFSET ?2")?;
    let rows = stmt.query_map(params![doc_id, offset.max(0), limit.clamp(1, 200)], |r| {
        let locator: String = r.get(1)?;
        Ok(ChunkView {
            seq: r.get(0)?,
            locator: serde_json::from_str(&locator).unwrap_or(Locator::None),
            text: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 关键词检索：在给定的文件夹范围（已展开子文件夹）内，返回（片段编号）按相关度排序。
pub fn keyword_search(c: &Connection, query: &str, folders: &[String], limit: usize) -> Result<Vec<i64>> {
    let Some(q) = chunk::fts_query(query) else { return Ok(Vec::new()) };
    let mut stmt = c.prepare(
        "SELECT c.id FROM chunks_fts
         JOIN chunks c ON c.id = chunks_fts.rowid
         JOIN documents d ON d.id = c.document_id
         WHERE chunks_fts MATCH ?1 AND d.status = 'ready'
           AND d.folder_id IN (SELECT value FROM json_each(?2))
         ORDER BY bm25(chunks_fts) LIMIT ?3",
    )?;
    let rows =
        stmt.query_map(params![q, serde_json::to_string(folders).unwrap_or_default(), limit as i64], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 检索命中的一个片段，带上出处。
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub document_id: String,
    pub document: String,
    pub folder: String,
    pub locator: Locator,
    pub text: String,
}

pub fn load_hits(c: &Connection, chunk_ids: &[i64]) -> Result<Vec<Hit>> {
    let mut stmt = c.prepare(
        "SELECT c.id, c.document_id, d.name, d.folder_id, c.locator, c.text FROM chunks c JOIN documents d ON d.id = c.document_id WHERE c.id = ?1",
    )?;
    let mut out = Vec::new();
    for id in chunk_ids {
        let row = stmt
            .query_row([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .optional()?;
        if let Some((chunk_id, document_id, document, folder_id, locator, text)) = row {
            out.push(Hit {
                chunk_id,
                document_id,
                document,
                folder: folder_path(c, &folder_id)?,
                locator: serde_json::from_str(&locator).unwrap_or(Locator::None),
                text,
            });
        }
    }
    Ok(out)
}

// ---------- 设置 ----------

/// 知识库用到的两个模型：向量化、文字识别。没配置时相应功能降级，而不是报错。
#[derive(Debug, Clone, Default)]
pub struct KbModels {
    pub embed: Option<(Provider, String)>,
    pub ocr: Option<(Provider, String)>,
}

fn model_setting(c: &Connection, prefix: &str) -> Result<Option<(Provider, String)>> {
    let (Some(pid), Some(model)) =
        (store::get_setting(c, &format!("{prefix}_provider"))?, store::get_setting(c, &format!("{prefix}_model"))?)
    else {
        return Ok(None);
    };
    if model.is_empty() {
        return Ok(None);
    }
    // 对应的推理服务被删掉了，视为没配置
    Ok(store::get_provider(c, &pid).ok().map(|p| (p, model)))
}

pub fn models(c: &Connection) -> Result<KbModels> {
    Ok(KbModels { embed: model_setting(c, "kb.embed")?, ocr: model_setting(c, "kb.ocr")? })
}

/// 保存其中一个模型的选择；`provider_id` 为空表示不使用。返回这次是否真的改了。
pub fn set_model(c: &Connection, which: &str, provider_id: &str, model: &str) -> Result<bool> {
    let prefix = match which {
        "embed" => "kb.embed",
        "ocr" => "kb.ocr",
        _ => return Err(Error::Invalid("which".into())),
    };
    let (provider_id, model) = (provider_id.trim(), model.trim());
    if provider_id.is_empty() != model.is_empty() {
        return Err(Error::Invalid("model".into()));
    }
    if !provider_id.is_empty() {
        store::get_provider(c, provider_id)?;
    }
    let before =
        (store::get_setting(c, &format!("{prefix}_provider"))?, store::get_setting(c, &format!("{prefix}_model"))?);
    store::set_setting(c, &format!("{prefix}_provider"), provider_id)?;
    store::set_setting(c, &format!("{prefix}_model"), model)?;
    // 从没设置过和设置为空是一回事，不算改动
    let before = (before.0.unwrap_or_default(), before.1.unwrap_or_default());
    Ok(before != (provider_id.to_string(), model.to_string()))
}

/// 向量化模型换了之后，已入库的文档要重新向量化（保留片段，不重新解析和识别）。
pub fn requeue_all_for_embedding(c: &Connection) -> Result<usize> {
    c.execute("UPDATE chunks SET vector = NULL", [])?;
    Ok(c.execute(
        "UPDATE documents SET status = 'queued', resume_from = 'embed', embed_model = NULL, done = 0, total = 0, updated_at = ?1 WHERE status = 'ready'",
        [now_ms()],
    )?)
}

/// 文字识别模型配置好之后，把之前因为没有它而失败或跳过了扫描页的文档重新处理。
pub fn requeue_waiting_for_ocr(c: &Connection) -> Result<usize> {
    Ok(c.execute(
        "UPDATE documents SET status = 'queued', resume_from = 'parse', error_code = NULL, error_detail = NULL, done = 0, total = 0, updated_at = ?1
         WHERE (status = 'failed' AND error_code = 'ocr_not_configured') OR (status IN ('ready', 'queued') AND ocr_skipped > 0)",
        [now_ms()],
    )?)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub documents: i64,
    pub chunks: i64,
    pub queued: i64,
    pub failed: i64,
    pub ready: i64,
    /// 已入库但还没有向量（只能按关键词查）的文档数。
    pub keyword_only: i64,
}

pub fn stats(c: &Connection, folders: Option<&[String]>) -> Result<Stats> {
    let filter = match folders {
        Some(f) => serde_json::to_string(f).unwrap_or_default(),
        None => String::new(),
    };
    Ok(c.query_row(
        "SELECT count(*), coalesce(sum(chunks), 0),
                coalesce(sum(status IN ('queued', 'parsing', 'ocr', 'embedding')), 0),
                coalesce(sum(status = 'failed'), 0), coalesce(sum(status = 'ready'), 0),
                coalesce(sum(status = 'ready' AND embed_model IS NULL), 0)
         FROM documents WHERE ?1 = '' OR folder_id IN (SELECT value FROM json_each(?1))",
        [filter],
        |r| {
            Ok(Stats {
                documents: r.get(0)?,
                chunks: r.get(1)?,
                queued: r.get(2)?,
                failed: r.get(3)?,
                ready: r.get(4)?,
                keyword_only: r.get(5)?,
            })
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn ch(text: &str) -> Chunk {
        Chunk { locator: Locator::Page(1), text: text.into() }
    }

    #[tokio::test]
    async fn folders_nest_move_and_refuse_cycles() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let a = create_folder(c, "招标文件", None)?;
            let b = create_folder(c, "2026 政务云", Some(&a))?;
            let d = create_folder(c, "更正公告", Some(&b))?;
            assert!(matches!(create_folder(c, "招标文件", None), Err(Error::Invalid(m)) if m == "name_taken"));
            assert!(matches!(create_folder(c, " ", None), Err(Error::Invalid(_))));
            assert!(matches!(create_folder(c, "x", Some("nope")), Err(Error::NotFound(_))));
            assert_eq!(folder_path(c, &d)?, "招标文件 / 2026 政务云 / 更正公告");
            let mut sub = subtree(c, std::slice::from_ref(&a))?;
            sub.sort();
            let mut all = vec![a.clone(), b.clone(), d.clone()];
            all.sort();
            assert_eq!(sub, all);
            // 不能移到自己的子文件夹下
            assert!(matches!(update_folder(c, &a, None, Some(Some(&d))), Err(Error::Invalid(m)) if m == "parent"));
            // 移到最上层并改名
            update_folder(c, &d, Some("公告"), Some(None))?;
            assert_eq!(folder_path(c, &d)?, "公告");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn keyword_search_respects_folder_scope_and_status() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let a = create_folder(c, "售后", None)?;
            let b = create_folder(c, "招标", None)?;
            for (id, folder, text) in [
                ("d1", &a, "屏幕属于主要部件，保修二十四个月。"),
                ("d2", &b, "投标保证金为人民币五万元，保修期另行约定。"),
            ] {
                add_document(c, id, folder, &format!("{id}.txt"), 10)?;
                replace_chunks(c, id, &[ch(text), ch("无关内容")], Some(1), 0, 0)?;
            }
            // 还没处理完的文档搜不到
            assert!(keyword_search(c, "保修", &[a.clone(), b.clone()], 10)?.is_empty());
            c.execute("UPDATE documents SET status = 'ready'", [])?;

            let hits = load_hits(c, &keyword_search(c, "保修多久", std::slice::from_ref(&a), 10)?)?;
            assert_eq!(hits.len(), 1);
            assert_eq!((hits[0].document.as_str(), hits[0].folder.as_str()), ("d1.txt", "售后"));
            assert_eq!(keyword_search(c, "保修", &[a.clone(), b.clone()], 10)?.len(), 2);
            assert_eq!(keyword_search(c, "投标保证金", std::slice::from_ref(&a), 10)?.len(), 0);

            // 删除文档后全文索引也清掉；重新切分会替换旧片段
            delete_document(c, "d1")?;
            assert!(keyword_search(c, "屏幕", std::slice::from_ref(&a), 10)?.is_empty());
            replace_chunks(c, "d2", &[ch("全新内容")], None, 0, 0)?;
            assert!(keyword_search(c, "保证金", std::slice::from_ref(&b), 10)?.is_empty());
            assert_eq!(keyword_search(c, "全新", &[b], 10)?.len(), 1);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn queue_claims_in_order_and_honours_pause() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let f = create_folder(c, "f", None)?;
            assert!(matches!(add_document(c, "z", &f, "a.zip", 1), Err(Error::Doc("doc_unsupported", _))));
            add_document(c, "d1", &f, "a.txt", 1)?;
            add_document(c, "d2", &f, "b.PDF", 1)?;
            pause(c, "d1")?;
            let got = claim_next(c)?.unwrap();
            assert_eq!((got.id.as_str(), got.ext.as_str()), ("d2", "pdf"));
            assert!(claim_next(c)?.is_none());
            // 处理中被暂停：进度更新返回 false，结果不会被写成完成
            pause(c, "d2")?;
            assert!(!progress(c, "d2", "embedding", 1, 2)?);
            finish(c, "d2", None)?;
            assert_eq!(get_document(c, "d2")?.status, "paused");
            resume(c, "d1")?;
            assert_eq!(claim_next(c)?.unwrap().id, "d1");
            // 程序重启后，处理到一半的重新排队
            assert_eq!(requeue_interrupted(c)?, 1);
            let s = stats(c, None)?;
            assert_eq!((s.documents, s.queued), (2, 1));
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deleting_a_folder_reports_its_documents() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let a = create_folder(c, "a", None)?;
            let b = create_folder(c, "b", Some(&a))?;
            add_document(c, "d1", &a, "a.txt", 1)?;
            add_document(c, "d2", &b, "b.txt", 1)?;
            replace_chunks(c, "d2", &[ch("内容")], None, 0, 0)?;
            let mut gone = delete_folder(c, &a)?;
            gone.sort();
            assert_eq!(gone, vec!["d1", "d2"]);
            let left: i64 =
                c.query_row("SELECT (SELECT count(*) FROM documents) + (SELECT count(*) FROM chunks)", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
    }
}
