//! 对话里上传的文件：读出文字后只在这个对话里使用，不进知识库。

use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::kb::parse::{self, Kind, Locator, Section};
use crate::kb::KbModels;
use crate::llm::{Endpoint, LlmClient};
use crate::web::best_passages;
use crate::{new_id, now_ms, Error, Result};

/// 扫描件最多识别多少页，避免一次上传让对话等太久。
const MAX_OCR_PAGES: usize = 30;
/// 一份文件最多保留多少字。
const MAX_TEXT_CHARS: usize = 400_000;
/// 一个对话里所有文件带给模型的总字数上限。
pub const CONTEXT_BUDGET: usize = 16_000;

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: String,
    pub conversation_id: String,
    /// 随哪条消息发出；还没发出时为空。
    pub message_id: Option<String>,
    pub name: String,
    pub size: i64,
    /// 读出的文字有多少字。
    pub chars: i64,
    #[serde(skip_serializing)]
    pub text: String,
    /// 有多少扫描页因为没有文字识别模型或超出页数上限而没读到。
    pub skipped_pages: i64,
    pub created_at: i64,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    let text: String = r.get("text")?;
    Ok(Attachment {
        id: r.get("id")?,
        conversation_id: r.get("conversation_id")?,
        message_id: r.get("message_id")?,
        name: r.get("name")?,
        size: r.get("size")?,
        chars: text.chars().count() as i64,
        text,
        skipped_pages: r.get("skipped_pages")?,
        created_at: r.get("created_at")?,
    })
}

pub fn add(
    c: &Connection,
    conversation_id: &str,
    name: &str,
    size: i64,
    text: &str,
    skipped_pages: i64,
) -> Result<Attachment> {
    crate::store::get_conversation(c, conversation_id)?;
    let id = new_id();
    let name: String = name.trim().chars().take(200).collect();
    c.execute(
        "INSERT INTO attachments(id, conversation_id, name, size, text, skipped_pages, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, conversation_id, name, size, text, skipped_pages, now_ms()],
    )?;
    Ok(c.query_row("SELECT * FROM attachments WHERE id = ?1", [&id], from_row)?)
}

pub fn list(c: &Connection, conversation_id: &str) -> Result<Vec<Attachment>> {
    let mut stmt = c.prepare("SELECT * FROM attachments WHERE conversation_id = ?1 ORDER BY created_at, rowid")?;
    let rows = stmt.query_map([conversation_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 删除一个还没发出的文件。
pub fn remove_pending(c: &Connection, conversation_id: &str, id: &str) -> Result<()> {
    match c.execute(
        "DELETE FROM attachments WHERE id = ?1 AND conversation_id = ?2 AND message_id IS NULL",
        params![id, conversation_id],
    )? {
        0 => Err(Error::NotFound("attachment")),
        _ => Ok(()),
    }
}

/// 把还没发出的文件挂到刚发出的这条消息上。
pub fn attach_pending(c: &Connection, conversation_id: &str, message_id: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        c.execute(
            "UPDATE attachments SET message_id = ?3 WHERE id = ?1 AND conversation_id = ?2 AND message_id IS NULL",
            params![id, conversation_id, message_id],
        )?;
    }
    // 没被这条消息带上的，是用户发之前移除又没删干净的，清掉
    c.execute("DELETE FROM attachments WHERE conversation_id = ?1 AND message_id IS NULL", [conversation_id])?;
    Ok(())
}

/// 把对话里的文件整理成带给模型的资料：(文件, 这次带上的文字)。
/// 总量不大时全文带上；超出预算时每份文件只带与问题最相关的几段。
pub fn context(files: &[Attachment], query: &str) -> Vec<(Attachment, String)> {
    let total: usize = files.iter().map(|f| f.chars as usize).sum();
    if total <= CONTEXT_BUDGET {
        return files.iter().map(|f| (f.clone(), f.text.clone())).collect();
    }
    let each = (CONTEXT_BUDGET / files.len().max(1)).max(800);
    files
        .iter()
        .map(|f| {
            (f.clone(), if (f.chars as usize) <= each { f.text.clone() } else { best_passages(&f.text, query, each) })
        })
        .collect()
}

fn join(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections {
        let label = match &s.locator {
            Locator::Page(n) => format!("【第 {n} 页】\n"),
            Locator::Slide(n) => format!("【第 {n} 张幻灯片】\n"),
            Locator::Sheet(name) => format!("【工作表 {name}】\n"),
            Locator::Heading(_) | Locator::None => String::new(),
        };
        if s.text.trim().is_empty() {
            continue;
        }
        out.push_str(&label);
        out.push_str(s.text.trim_end());
        out.push_str("\n\n");
    }
    out.trim_end().chars().take(MAX_TEXT_CHARS).collect()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::Doc("doc_damaged", e.to_string()))?
}

/// 读出一个上传文件里的文字。返回（文字，没读到的扫描页数）。
pub async fn extract(
    llm: &LlmClient,
    models: &KbModels,
    pdfium: Option<&Path>,
    path: &Path,
    ext: &str,
) -> Result<(String, i64)> {
    let kind = parse::kind_of(ext).ok_or_else(|| Error::Doc("doc_unsupported", ext.to_string()))?;
    let ocr = models
        .ocr
        .as_ref()
        .map(|(p, m)| (Endpoint { base_url: p.base_url.clone(), api_key: p.api_key.clone() }, m.clone()));
    let mut skipped = 0i64;
    let sections: Vec<Section> = match kind {
        Kind::Pdf => {
            let lib = pdfium.ok_or_else(|| Error::Doc("pdf_engine_missing", String::new()))?.to_path_buf();
            let (l, p) = (lib.clone(), path.to_path_buf());
            let mut texts = blocking(move || parse::pdf_pages(&l, &p)).await?;
            let scanned: Vec<usize> =
                texts.iter().enumerate().filter(|(_, t)| t.trim().chars().count() < 16).map(|(i, _)| i).collect();
            match &ocr {
                None if scanned.len() == texts.len() => return Err(Error::Doc("ocr_not_configured", String::new())),
                None => skipped = scanned.len() as i64,
                Some((ep, model)) => {
                    skipped = scanned.len().saturating_sub(MAX_OCR_PAGES) as i64;
                    for &i in scanned.iter().take(MAX_OCR_PAGES) {
                        let (l, p) = (lib.clone(), path.to_path_buf());
                        let png = blocking(move || parse::pdf_render_page(&l, &p, i)).await?;
                        texts[i] = llm.ocr_image(ep, model, &png).await?;
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
            let (ep, model) = ocr.ok_or_else(|| Error::Doc("ocr_not_configured", String::new()))?;
            let p = path.to_path_buf();
            let png = blocking(move || parse::image_for_ocr(&p)).await?;
            vec![Section { locator: Locator::None, text: llm.ocr_image(&ep, &model, &png).await? }]
        }
        other => {
            let p = path.to_path_buf();
            blocking(move || parse::parse(&p, other)).await?
        }
    };
    let text = join(&sections);
    if text.trim().is_empty() {
        return Err(Error::Doc("doc_empty", String::new()));
    }
    Ok((text, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent, store, Db};

    #[tokio::test]
    async fn pending_files_attach_to_the_message_and_die_with_the_conversation() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let conv = store::create_conversation(c, agent::GENERAL_AGENT_ID)?;
            let a = add(c, &conv.id, " 发票.pdf ", 10, "发票日期 2025 年 9 月 2 日", 0)?;
            let b = add(c, &conv.id, "报价.xlsx", 20, "换屏 600 元", 0)?;
            let dropped = add(c, &conv.id, "不要了.txt", 5, "x", 0)?;
            assert_eq!((a.name.as_str(), a.chars), ("发票.pdf", 19));
            assert!(!serde_json::to_string(&a).unwrap().contains("发票日期"), "text is not sent back to the interface");
            assert!(matches!(add(c, "nope", "x.txt", 1, "x", 0), Err(Error::NotFound(_))));

            remove_pending(c, &conv.id, &dropped.id)?;
            let msg = store::add_user_message(c, &conv.id, "看看这两个文件", "p", "m")?;
            attach_pending(c, &conv.id, &msg.id, &[a.id.clone(), b.id.clone()])?;
            let files = list(c, &conv.id)?;
            assert_eq!(files.len(), 2);
            assert!(files.iter().all(|f| f.message_id.as_deref() == Some(msg.id.as_str())));
            // 已经发出的不能再删
            assert!(remove_pending(c, &conv.id, &a.id).is_err());

            store::delete_conversation(c, &conv.id)?;
            let left: i64 = c.query_row("SELECT count(*) FROM attachments", [], |r| r.get(0))?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
    }

    fn file(name: &str, text: String) -> Attachment {
        Attachment {
            id: name.into(),
            conversation_id: "c".into(),
            message_id: None,
            name: name.into(),
            size: 0,
            chars: text.chars().count() as i64,
            text,
            skipped_pages: 0,
            created_at: 0,
        }
    }

    #[test]
    fn small_files_go_in_whole_and_large_ones_are_narrowed_to_the_question() {
        let small = vec![file("a.txt", "保修两年".into())];
        assert_eq!(context(&small, "保修")[0].1, "保修两年");

        let filler = "这一句和问题没有关系，只是用来占位置的内容。\n".repeat(900);
        let big = vec![
            file("big.txt", format!("{filler}\n投标保证金为人民币五万元，须在开标前缴纳。\n{filler}")),
            file("b.txt", "短文件".into()),
        ];
        let got = context(&big, "投标保证金是多少");
        assert!(got[0].1.contains("投标保证金为人民币五万元"));
        assert!(got[0].1.chars().count() <= CONTEXT_BUDGET / 2);
        assert_eq!(got[1].1, "短文件");
    }
}
