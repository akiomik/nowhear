use thiserror::Error;

/// Errors that can occur when using the media watcher.
#[derive(Error, Debug)]
pub enum MediaWatcherError {
    /// The requested player was not found or is not currently running.
    #[error("Player not found: {0}")]
    PlayerNotFound(String),

    /// Failed to connect to the underlying media service.
    #[error("Failed to connect to media service: {0}")]
    ConnectionError(String),

    /// Failed to parse media information from the system.
    #[error("Failed to parse media information: {0}")]
    ParseError(String),

    /// The current platform is not supported.
    #[error("Platform not supported")]
    UnsupportedPlatform,

    /// An internal error occurred.
    #[error("Internal error: {0}")]
    InternalError(String),
}

/// Result type alias for media watcher operations.
pub type Result<T> = std::result::Result<T, MediaWatcherError>;
