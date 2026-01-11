//! # media-watcher
//!
//! Cross-platform library for monitoring media playback information.
//!
//! ## Features
//!
//! - Get currently playing media information across Linux, macOS, and Windows
//! - Subscribe to media events via async streams
//! - Unified API across all platforms
//!
//! ## Example
//!
//! ```no_run
//! use nowhear::MediaWatcherBuilder;
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let watcher = MediaWatcherBuilder::new().build().await?;
//!     
//!     // Subscribe to events
//!     let mut stream = watcher.event_stream().await?;
//!     while let Some(event) = stream.next().await {
//!         println!("Event: {:?}", event);
//!     }
//!     
//!     Ok(())
//! }
//! ```

pub mod error;
pub mod types;
pub mod watcher;

mod platform;

// Re-export main types
pub use error::{MediaWatcherError, Result};
pub use types::{MediaEvent, PlaybackState, PlayerInfo, Track};
pub use watcher::{MediaWatcher, MediaWatcherBuilder};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_creation() {
        let track = Track::unknown();
        assert_eq!(track.title, "Unknown");
    }

    #[test]
    fn test_playback_state() {
        let state = PlaybackState::Playing;
        assert_eq!(state, PlaybackState::Playing);
    }
}
