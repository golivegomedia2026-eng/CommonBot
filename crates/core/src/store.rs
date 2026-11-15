//! 数据读写。函数都是同步的，放在 `Db::run` 的闭包里调用。

use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{new_id, now_ms, Error, Result};

// ---------- 设置 ----------

pub fn get_setting(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ---------- 访问码 ----------

/// 易读的字符集，去掉了 0/O、1/I/L 这类容易看错的字符。
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn random_string(alphabet: &[u8], len: usize) -> String {
    let mut rng = rand::rngs::OsRng;
    (0..len).map(|_| alphabet[rng.gen_range(0..alphabet.len())] as char).collect()
}

/// 其他设备连接时输入的访问码；首次调用时生成。
pub fn access_code(c: &Connection) -> Result<String> {
    if let Some(code) = get_setting(c, "access_code")? {
        return Ok(code);
    }
    reset_access_code(c)
}

/// 换一个访问码；已登录的设备会失效，需要重新输入。
pub fn reset_access_code(c: &Connection) -> Result<String> {
    let code = random_string(CODE_ALPHABET, 8);
    set_setting(c, "access_code", &code)?;
    Ok(code)
}

/// 输入正确访问码后发给浏览器的凭据。由访问码和本机密钥算出，换访问码即失效。
pub fn access_token(c: &Connection) -> Result<String> {
    let secret = match get_setting(c, "access_secret")? {
        Some(s) => s,
        None => {
            let s = random_string(b"0123456789abcdef", 48);
            set_setting(c, "access_secret", &s)?;
            s
        }
    };
    let code = access_code(c)?;
    let digest = Sha256::digest(format!("cb1:{secret}:{code}").as_bytes());
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

// ---------- 推理后端 ----------

#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    /// `local`：本机或局域网里自己的推理服务；`cloud`：云端模型。
    pub kind: String,
    pub base_url: String,
    /// 密钥不返回给界面，只告诉界面有没有。
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub has_key: bool,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInput {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    /// 不传：保持原样；空字符串：清除；其他：替换。
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

impl ProviderInput {
    fn validated(mut self) -> Result<Self> {
        self.name = self.name.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if self.name.is_empty() || self.name.chars().count() > 60 {
            return Err(Error::Invalid("name".into()));
        }
        if self.kind != "local" && self.kind != "cloud" {
            return Err(Error::Invalid("kind".into()));
        }
        match reqwest::Url::parse(&self.base_url) {
            Ok(u) if (u.scheme() == "http" || u.scheme() == "https") && u.has_host() => {}
            _ => return Err(Error::Invalid("base_url".into())),
        }
        self.default_model = self.default_model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
        Ok(self)
    }
}

fn provider_from_row(r: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let api_key: Option<String> = r.get("api_key")?;
    Ok(Provider {
        id: r.get("id")?,
        name: r.get("name")?,
        kind: r.get("kind")?,
        base_url: r.get("base_url")?,
        has_key: api_key.as_deref().is_some_and(|k| !k.is_empty()),
        api_key,
        default_model: r.get("default_model")?,
    })
}

pub fn list_providers(c: &Connection) -> Result<Vec<Provider>> {
    let mut stmt = c.prepare("SELECT * FROM providers ORDER BY created_at, id")?;
    let rows = stmt.query_map([], provider_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_provider(c: &Connection, id: &str) -> Result<Provider> {
    c.query_row("SELECT * FROM providers WHERE id = ?1", [id], provider_from_row)
        .optional()?
        .ok_or(Error::NotFound("provider"))
}

pub fn create_provider(c: &Connection, input: ProviderInput) -> Result<Provider> {
    let input = input.validated()?;
    let id = new_id();
    c.execute(
        "INSERT INTO providers(id, name, kind, base_url, api_key, default_model, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, input.name, input.kind, input.base_url, input.api_key.filter(|k| !k.is_empty()), input.default_model, now_ms()],
    )?;
    get_provider(c, &id)
}

pub fn update_provider(c: &Connection, id: &str, input: ProviderInput) -> Result<Provider> {
    let input = input.validated()?;
    let existing = get_provider(c, id)?;
    let api_key = match input.api_key {
        None => existing.api_key,
        Some(k) if k.is_empty() => None,
        Some(k) => Some(k),
    };
    c.execute(
        "UPDATE providers SET name = ?2, kind = ?3, base_url = ?4, api_key = ?5, default_model = ?6 WHERE id = ?1",
        params![id, input.name, input.kind, input.base_url, api_key, input.default_model],
    )?;
    get_provider(c, id)
}

pub fn delete_provider(c: &Connection, id: &str) -> Result<()> {
    match c.execute("DELETE FROM providers WHERE id = ?1", [id])? {
        0 => Err(Error::NotFound("provider")),
        _ => Ok(()),
    }
}

/// 第一次启动时放入一个指向本机 Ollama 的默认后端，开箱即可检测。
pub fn seed_defaults(c: &Connection) -> Result<()> {
    if get_setting(c, "seeded")?.is_some() {
        return Ok(());
    }
    create_provider(
        c,
        ProviderInput {
            name: "Ollama".into(),
            kind: "local".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key: None,
            default_model: None,
        },
    )?;
    set_setting(c, "seeded", "1")
}

// ---------- 对话 ----------

#[derive(Debug, Clone, Serialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub reasoning: String,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: i64,
}

fn conversation_from_row(r: &rusqlite::Row) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: r.get("id")?,
        title: r.get("title")?,
        provider_id: r.get("provider_id")?,
        model: r.get("model")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn message_from_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get("id")?,
        conversation_id: r.get("conversation_id")?,
        role: r.get("role")?,
        content: r.get("content")?,
        reasoning: r.get("reasoning")?,
        status: r.get("status")?,
        error_code: r.get("error_code")?,
        created_at: r.get("created_at")?,
    })
}

pub fn create_conversation(c: &Connection) -> Result<Conversation> {
    let (id, now) = (new_id(), now_ms());
    c.execute("INSERT INTO conversations(id, created_at, updated_at) VALUES (?1, ?2, ?2)", params![id, now])?;
    get_conversation(c, &id)
}

pub fn get_conversation(c: &Connection, id: &str) -> Result<Conversation> {
    c.query_row("SELECT * FROM conversations WHERE id = ?1", [id], conversation_from_row)
        .optional()?
        .ok_or(Error::NotFound("conversation"))
}

pub fn list_conversations(c: &Connection) -> Result<Vec<Conversation>> {
    let mut stmt = c.prepare("SELECT * FROM conversations ORDER BY updated_at DESC, id LIMIT 500")?;
    let rows = stmt.query_map([], conversation_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn rename_conversation(c: &Connection, id: &str, title: &str) -> Result<Conversation> {
    let title: String = title.trim().chars().take(80).collect();
    if title.is_empty() {
        return Err(Error::Invalid("title".into()));
    }
    match c.execute("UPDATE conversations SET title = ?2 WHERE id = ?1", params![id, title])? {
        0 => Err(Error::NotFound("conversation")),
        _ => get_conversation(c, id),
    }
}

pub fn delete_conversation(c: &Connection, id: &str) -> Result<()> {
    match c.execute("DELETE FROM conversations WHERE id = ?1", [id])? {
        0 => Err(Error::NotFound("conversation")),
        _ => Ok(()),
    }
}

pub fn list_messages(c: &Connection, conversation_id: &str) -> Result<Vec<Message>> {
    let mut stmt = c.prepare("SELECT * FROM messages WHERE conversation_id = ?1 ORDER BY created_at, rowid")?;
    let rows = stmt.query_map([conversation_id], message_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 记下用户的一句话，并更新对话用的后端、模型和标题（标题取第一句话的开头）。
pub fn add_user_message(
    c: &mut Connection,
    conversation_id: &str,
    content: &str,
    provider_id: &str,
    model: &str,
) -> Result<Message> {
    let tx = c.transaction()?;
    let conv = get_conversation(&tx, conversation_id)?;
    let (id, now) = (new_id(), now_ms());
    tx.execute(
        "INSERT INTO messages(id, conversation_id, role, content, created_at) VALUES (?1, ?2, 'user', ?3, ?4)",
        params![id, conversation_id, content, now],
    )?;
    let title = if conv.title.is_empty() {
        let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        line.chars().take(30).collect()
    } else {
        conv.title
    };
    tx.execute(
        "UPDATE conversations SET title = ?2, provider_id = ?3, model = ?4, updated_at = ?5 WHERE id = ?1",
        params![conversation_id, title, provider_id, model, now],
    )?;
    let msg = tx.query_row("SELECT * FROM messages WHERE id = ?1", [&id], message_from_row)?;
    tx.commit()?;
    Ok(msg)
}

pub fn add_assistant_message(
    c: &Connection,
    conversation_id: &str,
    content: &str,
    reasoning: &str,
    status: &str,
    error_code: Option<&str>,
) -> Result<Message> {
    let (id, now) = (new_id(), now_ms());
    c.execute(
        "INSERT INTO messages(id, conversation_id, role, content, reasoning, status, error_code, created_at) VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, ?6, ?7)",
        params![id, conversation_id, content, reasoning, status, error_code, now],
    )?;
    c.execute("UPDATE conversations SET updated_at = ?2 WHERE id = ?1", params![conversation_id, now])?;
    Ok(c.query_row("SELECT * FROM messages WHERE id = ?1", [&id], message_from_row)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn input(url: &str) -> ProviderInput {
        ProviderInput {
            name: " 测试 ".into(),
            kind: "local".into(),
            base_url: url.into(),
            api_key: Some("sk-1".into()),
            default_model: None,
        }
    }

    #[tokio::test]
    async fn provider_key_is_kept_cleared_and_never_serialized() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let p = create_provider(c, input("http://127.0.0.1:1/v1/"))?;
            assert_eq!(p.name, "测试");
            assert_eq!(p.base_url, "http://127.0.0.1:1/v1");
            assert!(p.has_key);
            let json = serde_json::to_string(&p).unwrap();
            assert!(!json.contains("sk-1"), "{json}");

            let mut keep = input("http://127.0.0.1:1/v1");
            keep.api_key = None;
            assert_eq!(update_provider(c, &p.id, keep)?.api_key.as_deref(), Some("sk-1"));

            let mut clear = input("http://127.0.0.1:1/v1");
            clear.api_key = Some(String::new());
            assert!(!update_provider(c, &p.id, clear)?.has_key);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn rejects_bad_provider_input() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            for url in ["", "ftp://x", "not a url", "file:///etc/passwd"] {
                assert!(matches!(create_provider(c, input(url)), Err(Error::Invalid(_))), "{url}");
            }
            let mut bad = input("http://x/v1");
            bad.kind = "other".into();
            assert!(matches!(create_provider(c, bad), Err(Error::Invalid(_))));
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn conversation_title_and_cascade_delete() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let conv = create_conversation(c)?;
            add_user_message(
                c,
                &conv.id,
                "\n  第一句话很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长\n第二行",
                "p",
                "m",
            )?;
            add_assistant_message(c, &conv.id, "好", "", "ok", None)?;
            let conv = get_conversation(c, &conv.id)?;
            assert_eq!(conv.title.chars().count(), 30);
            assert_eq!(conv.model.as_deref(), Some("m"));
            assert_eq!(list_messages(c, &conv.id)?.len(), 2);
            delete_conversation(c, &conv.id)?;
            let left: i64 = c.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn access_token_changes_with_the_code() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let code = access_code(c)?;
            assert_eq!(code.len(), 8);
            assert_eq!(access_code(c)?, code);
            let t1 = access_token(c)?;
            assert_eq!(access_token(c)?, t1);
            assert_ne!(reset_access_code(c)?, code);
            assert_ne!(access_token(c)?, t1);
            Ok(())
        })
        .await
        .unwrap();
    }
}
