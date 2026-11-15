//! CommonBot 服务：HTTP 接口 + 内嵌界面。

mod access;
mod assets;
mod routes;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{middleware, Json, Router};
use commonbot_core::llm::LlmClient;
use commonbot_core::{store, Db, Error};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub llm: LlmClient,
    /// 服务监听的地址，用来告诉界面是否对局域网开放。
    pub listen: SocketAddr,
    pub(crate) access_token: Arc<RwLock<String>>,
    pub(crate) access_failures: Arc<Mutex<access::Failures>>,
}

impl AppState {
    pub async fn new(db: Db, listen: SocketAddr) -> Result<Self, Error> {
        let token = db
            .run(|c| {
                store::seed_defaults(c)?;
                store::access_token(c)
            })
            .await?;
        Ok(Self {
            db,
            llm: LlmClient::new(),
            listen,
            access_token: Arc::new(RwLock::new(token)),
            access_failures: Arc::default(),
        })
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .nest("/api", routes::api())
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
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::Unreachable(_) | Error::Unauthorized | Error::Upstream { .. } => StatusCode::BAD_GATEWAY,
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
