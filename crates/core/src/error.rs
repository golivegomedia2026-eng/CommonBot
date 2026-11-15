use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// 按「用户能做什么」分类的错误。`code()` 是稳定的字符串，界面据此显示对应语言的说明。
#[derive(Debug, Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// 连不上推理后端（没启动、地址不对、网络不通）。
    #[error("cannot reach the model service: {0}")]
    Unreachable(String),
    /// 推理后端拒绝了密钥。
    #[error("the model service rejected the credentials")]
    Unauthorized,
    /// 推理后端返回了错误。
    #[error("the model service returned {status}: {message}")]
    Upstream { status: u16, message: String },
    /// 文档处理失败。第一个字段是稳定的原因代码。
    #[error("document problem ({0}): {1}")]
    Doc(&'static str, String),
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::Invalid(_) => "invalid",
            Error::Unreachable(_) => "model_unreachable",
            Error::Unauthorized => "model_unauthorized",
            Error::Upstream { .. } => "model_error",
            Error::Doc(code, _) => code,
            Error::Storage(_) => "storage",
            Error::Internal(_) => "internal",
        }
    }
}
