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

#[cfg(target_os = "linux")]
impl From<zbus::Error> for MediaSourceError {
    fn from(value: zbus::Error) -> Self {
        Self::ConnectionError(value.to_string())
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::absolute_paths)]
impl From<zbus::zvariant::Error> for MediaSourceError {
    fn from(value: zbus::zvariant::Error) -> Self {
        Self::ParseError(value.to_string())
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::absolute_paths)]
impl From<zbus::fdo::Error> for MediaSourceError {
    fn from(value: zbus::fdo::Error) -> Self {
        Self::ParseError(value.to_string())
    }
}

#[cfg(target_os = "macos")]
impl From<serde_json::Error> for MediaSourceError {
    fn from(value: serde_json::Error) -> Self {
        Self::ParseError(value.to_string())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_player_not_found_display() {
        let err = MediaSourceError::PlayerNotFound("spotify".to_string());
        assert_eq!(err.to_string(), "Player not found: spotify");
    }

    #[test]
    fn test_connection_error_display() {
        let err = MediaSourceError::ConnectionError("timeout".to_string());
        assert_eq!(
            err.to_string(),
            "Failed to connect to media service: timeout"
        );
    }

    #[test]
    fn test_parse_error_display() {
        let err = MediaSourceError::ParseError("invalid JSON".to_string());
        assert_eq!(
            err.to_string(),
            "Failed to parse media information: invalid JSON"
        );
    }

    #[test]
    fn test_unsupported_platform_display() {
        let err = MediaSourceError::UnsupportedPlatform;
        assert_eq!(err.to_string(), "Platform not supported");
    }

    #[test]
    fn test_internal_error_display() {
        let err = MediaSourceError::InternalError("unexpected state".to_string());
        assert_eq!(err.to_string(), "Internal error: unexpected state");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{invalid}")
            .expect_err("input is intentionally malformed JSON");
        let err = MediaSourceError::from(json_err);
        assert!(matches!(err, MediaSourceError::ParseError(_)));
    }
}
