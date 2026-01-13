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
//! - **Windows**: Uses Windows Media Control API (`GlobalSystemMediaTransportControlsSessionManager`)
//!
//! ## Basic Usage
//!
//! ### Listing Players
//!
//! ```no_run
//! use nowhear::{MediaSource, MediaSourceBuilder, Result};
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let source = MediaSourceBuilder::new().build().await?;
//!     let players = source.list_players().await?;
//!     println!("Available players: {:?}", players);
//!     Ok(())
//! }
//! ```
//!
//! ### Getting Player Information
//!
//! ```no_run
//! use nowhear::{MediaSource, MediaSourceBuilder, Result};
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let source = MediaSourceBuilder::new().build().await?;
//!     let player_info = source.get_player("spotify").await?;
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
//! use nowhear::{MediaSource, MediaSourceBuilder, Result};
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let source = MediaSourceBuilder::new().build().await?;
//!     let mut stream = source.event_stream().await?;
//!
//!     while let Some(event) = stream.next().await {
//!         println!("Event: {:?}", event);
//!     }
//!
//!     Ok(())
//! }
//! ```

pub mod error;
pub mod source;
pub mod types;

mod platform;

// Re-export main types
pub use error::{MediaSourceError, Result};
pub use source::{MediaSource, MediaSourceBuilder};
pub use types::{MediaEvent, PlaybackState, PlayerInfo, Track};

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
