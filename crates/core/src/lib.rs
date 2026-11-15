//! CommonBot 的业务核心：数据存储、推理后端访问。不依赖 HTTP 框架和界面。

pub mod agent;
pub mod attach;
pub mod build;
pub mod db;
pub mod error;
pub mod kb;
pub mod llm;
pub mod store;
pub mod update;
pub mod web;

pub use db::Db;
pub use error::{Error, Result};

/// 当前毫秒时间戳。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
