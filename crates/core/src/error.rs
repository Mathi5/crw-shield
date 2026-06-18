use thiserror::Error;

pub type Result<T> = std::result::Result<T, CrwError>;

#[derive(Debug, Error)]
pub enum CrwError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("fetch error: {0}")]
    Fetch(String),

    #[error("HTTP error: status {status}: {message}")]
    Http { status: u16, message: String },

    #[error("challenge detected: {0}")]
    Challenge(String),

    #[error("extraction error: {0}")]
    Extraction(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl CrwError {
    pub fn code(&self) -> &'static str {
        match self {
            CrwError::InvalidUrl(_) => "INVALID_URL",
            CrwError::Fetch(_) => "FETCH_ERROR",
            CrwError::Http { .. } => "HTTP_ERROR",
            CrwError::Challenge(_) => "CHALLENGE_DETECTED",
            CrwError::Extraction(_) => "EXTRACTION_ERROR",
            CrwError::Config(_) => "CONFIG_ERROR",
            CrwError::NotImplemented(_) => "NOT_IMPLEMENTED",
            CrwError::Internal(_) => "INTERNAL_ERROR",
            CrwError::Other(_) => "INTERNAL_ERROR",
        }
    }
}
