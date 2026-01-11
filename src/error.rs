use thiserror::Error;

#[derive(Error, Debug)]
pub enum MediaWatcherError {
    #[error("No active media player found")]
    NoPlayerFound,

    #[error("Player not found: {0}")]
    PlayerNotFound(String),

    #[error("Failed to connect to media service: {0}")]
    ConnectionError(String),

    #[error("Failed to parse media information: {0}")]
    ParseError(String),

    #[error("Platform not supported")]
    UnsupportedPlatform,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    InternalError(String),
}

pub type Result<T> = std::result::Result<T, MediaWatcherError>;
