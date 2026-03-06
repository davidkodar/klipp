#[derive(Debug, thiserror::Error)]
pub enum KlippError {
    #[error("media decode error: {0}")]
    Decode(String),

    #[error("GPU error: {0}")]
    Gpu(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("project error: {0}")]
    Project(String),

    #[error("not found: {0}")]
    NotFound(String),
}

pub type KlippResult<T> = Result<T, KlippError>;
