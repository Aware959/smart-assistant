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
