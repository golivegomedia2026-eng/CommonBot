//! CommonBot 服务：HTTP 接口 + 内嵌界面。

mod access;
mod assets;
mod chat;
mod published;
mod routes;
mod routes_kb;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{middleware, Json, Router};
use commonbot_core::kb::index::VectorIndex;
use commonbot_core::kb::ingest::Ingestor;
use commonbot_core::kb::search::Retriever;
use commonbot_core::llm::LlmClient;
use commonbot_core::{store, Db, Error};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub llm: LlmClient,
    /// 服务监听的地址，用来告诉界面是否对局域网开放。
    pub listen: SocketAddr,
    /// 知识库检索。
    pub retriever: Retriever,
    /// 知识库入库队列。
    pub ingestor: Arc<Ingestor>,
    /// 联网搜索。
    pub web: commonbot_core::web::WebClient,
    pub(crate) access_token: Arc<RwLock<String>>,
    pub(crate) access_failures: Arc<Mutex<access::Failures>>,
}

impl AppState {
    /// `data_dir` 下存放上传的原文件；`pdfium` 是 PDF 解析库的位置，没有时 PDF 无法入库。
    pub async fn new(db: Db, listen: SocketAddr, data_dir: PathBuf, pdfium: Option<PathBuf>) -> Result<Self, Error> {
        let token = db
            .run(|c| {
                store::seed_defaults(c)?;
                store::access_token(c)
            })
            .await?;
        let llm = LlmClient::new();
        let index = Arc::new(VectorIndex::default());
        let ingestor = Ingestor::new(db.clone(), llm.clone(), index.clone(), data_dir.join("files"), pdfium);
        ingestor.spawn();
        Ok(Self {
            retriever: Retriever { db: db.clone(), llm: llm.clone(), index },
            ingestor,
            // 读取网页时默认拒绝本机和内网地址；只有测试会放开
            web: commonbot_core::web::WebClient::new(
                std::env::var("COMMONBOT_WEB_ALLOW_PRIVATE").is_ok_and(|v| v == "1"),
            ),
            db,
            llm,
            listen,
            access_token: Arc::new(RwLock::new(token)),
            access_failures: Arc::default(),
        })
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .nest("/api", routes::api())
        .nest("/v1", published::openai())
        .fallback(assets::serve)
        .layer(middleware::from_fn_with_state(state.clone(), access::guard))
        .with_state(state)
}

/// 接口统一的错误返回：`{ "code": "...", "message": "..." }`。
/// `code` 稳定，界面据此显示对应语言的说明；`message` 是给排查问题用的原文。
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let status = match &e {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Invalid(_) | Error::Doc(..) => StatusCode::BAD_REQUEST,
            Error::Unreachable(_) | Error::Unauthorized | Error::Upstream { .. } | Error::Web(..) => {
                StatusCode::BAD_GATEWAY
            }
            Error::Storage(_) | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %e, "request failed");
        }
        Self { status, code: e.code(), message: e.to_string() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "code": self.code, "message": self.message }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
