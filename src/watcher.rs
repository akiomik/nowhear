use crate::error::Result;
use crate::types::{MediaEvent, PlayerInfo};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Type alias for event stream
pub type EventStream = Pin<Box<dyn Stream<Item = MediaEvent> + Send>>;

/// Main trait for media watching functionality
#[async_trait]
pub trait MediaWatcher: Send + Sync {
    /// List all available players
    async fn list_players(&self) -> Result<Vec<String>>;

    /// Get information for a specific player
    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo>;

    /// Create an event stream that yields media events
    /// The stream continues indefinitely until dropped
    async fn event_stream(&self) -> Result<EventStream>;
}

/// Builder for creating a MediaWatcher instance
pub struct MediaWatcherBuilder {
    // Future extensions: filter by player name, etc.
}

impl MediaWatcherBuilder {
    pub fn new() -> Self {
        Self {}
    }

    /// Build the platform-specific MediaWatcher
    pub async fn build(self) -> Result<Box<dyn MediaWatcher>> {
        #[cfg(target_os = "linux")]
        {
            Ok(Box::new(
                crate::platform::linux::LinuxMediaWatcher::new().await?,
            ))
        }

        #[cfg(target_os = "macos")]
        {
            Ok(Box::new(
                crate::platform::macos::MacOSMediaWatcher::new().await?,
            ))
        }

        #[cfg(target_os = "windows")]
        {
            Ok(Box::new(
                crate::platform::windows::WindowsMediaWatcher::new().await?,
            ))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(crate::error::MediaWatcherError::UnsupportedPlatform)
        }
    }
}

impl Default for MediaWatcherBuilder {
    fn default() -> Self {
        Self::new()
    }
}
