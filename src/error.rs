//! Error types for media source operations.
//!
//! This module defines the error types that can occur when interacting with
//! media players across different platforms.

use std::result;

use thiserror::Error;

/// Errors that can occur when using the media source.
#[derive(Clone, Error, Debug, PartialEq, Eq)]
pub enum MediaSourceError {
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

/// Result type alias for media source operations.
///
/// This is a convenience type alias that uses [`MediaSourceError`] as the error type.
/// Most functions in this crate return this type.
///
/// # Examples
///
/// ```
/// use nowhear::Result;
///
/// fn example_function() -> Result<String> {
///     Ok("success".to_string())
/// }
/// ```
pub type Result<T> = result::Result<T, MediaSourceError>;
