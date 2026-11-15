use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::{Error, Result};

/// SQLite 连接。查询在阻塞线程池里执行，不占用异步运行时的线程。
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// 每个元素是一次升级，下标 + 1 即升级后的版本号。只追加，不改已发布的条目。
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE providers (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('local', 'cloud')),
    base_url      TEXT NOT NULL,
    api_key       TEXT,
    default_model TEXT,
    created_at    INTEGER NOT NULL
);
CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL DEFAULT '',
    provider_id TEXT,
    model       TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE messages (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content         TEXT NOT NULL,
    reasoning       TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'stopped', 'error')),
    error_code      TEXT,
    created_at      INTEGER NOT NULL
);
CREATE INDEX messages_by_conversation ON messages(conversation_id, created_at);
"#];

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Internal(format!("create {}: {e}", dir.display())))?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|e| Error::Internal(format!("database task: {e}")))?
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let latest = MIGRATIONS.len() as i64;
    if current > latest {
        // 数据由更新的版本写过，旧程序继续写会损坏它。
        return Err(Error::Internal(format!(
            "data was written by a newer version (schema {current}, this build supports {latest})"
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrates_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/commonbot.db");
        let db = Db::open(&path).unwrap();
        db.run(|c| {
            c.execute("INSERT INTO settings(key, value) VALUES ('k', 'v')", [])?;
            Ok(())
        })
        .await
        .unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let v: String =
            db.run(|c| Ok(c.query_row("SELECT value FROM settings WHERE key='k'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(v, "v");
    }

    #[test]
    fn refuses_data_from_a_newer_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        drop(conn);
        assert!(Db::open(&path).is_err());
    }
}
