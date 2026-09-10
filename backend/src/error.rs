use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqlError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("model inference error: {0}")]
    Inference(String),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("not found: {0}")]
    NotFound(String),
}

impl From<genai::Error> for SqlError {
    fn from(e: genai::Error) -> Self {
        SqlError::Inference(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SqlError>;

/// UniFFI 边界使用的错误类型。Kotlin/Swift 侧会映射为一类异常。
/// `flat_error` 表示跨 FFI 时仅保留 `to_string()` 文本。
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    Sql { message: String },
}

pub type FfiResult<T> = std::result::Result<T, FfiError>;

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::Sql { message } => write!(f, "{message}"),
        }
    }
}

impl From<SqlError> for FfiError {
    fn from(e: SqlError) -> Self {
        FfiError::Sql {
            message: e.to_string(),
        }
    }
}

impl From<serde_json::Error> for FfiError {
    fn from(e: serde_json::Error) -> Self {
        FfiError::Sql {
            message: e.to_string(),
        }
    }
}