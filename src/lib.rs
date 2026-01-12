//! # nowhear
//!
//! Cross-platform library for monitoring media playback information.
//!
//! This library provides a unified API to monitor media players across Linux, macOS, and Windows,
//! allowing you to retrieve current track information and subscribe to playback events.
//!
//! ## Platform Support
//!
//! - **Linux**: Uses MPRIS D-Bus interface
//! - **macOS**: Uses AppleScript to query Music.app and Spotify
//! - **Windows**: Uses Windows Media Control API (GlobalSystemMediaTransportControlsSessionManager)
//!
//! ## Basic Usage
//!
//! ### Listing Players
//!
//! ```no_run
//! use nowhear::{MediaWatcher, MediaWatcherBuilder};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let watcher = MediaWatcherBuilder::new().build().await?;
//!     let players = watcher.list_players().await?;
//!     println!("Available players: {:?}", players);
//!     Ok(())
//! }
//! ```
//!
//! ### Getting Player Information
//!
//! ```no_run
//! use nowhear::{MediaWatcher, MediaWatcherBuilder};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let watcher = MediaWatcherBuilder::new().build().await?;
//!     let player_info = watcher.get_player("spotify").await?;
//!
//!     if let Some(track) = player_info.current_track {
//!         println!("Now playing: {} by {}", track.title, track.artist.join(", "));
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Subscribing to Events
//!
//! ```no_run
//! use nowhear::{MediaWatcher, MediaWatcherBuilder};
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let watcher = MediaWatcherBuilder::new().build().await?;
//!     let mut stream = watcher.event_stream().await?;
//!
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
