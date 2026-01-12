use crate::error::Result;
use crate::types::{MediaEvent, PlayerInfo};
use futures::stream::BoxStream;

/// Type alias for event stream
pub type EventStream = BoxStream<'static, MediaEvent>;

/// Main trait for media watching functionality
pub trait MediaWatcher: Send + Sync {
    /// List all available players
    fn list_players(&self) -> impl std::future::Future<Output = Result<Vec<String>>> + Send;

    /// Get information for a specific player
    fn get_player(
        &self,
        player_name: &str,
    ) -> impl std::future::Future<Output = Result<PlayerInfo>> + Send;

    /// Create an event stream that yields media events
    /// The stream continues indefinitely until dropped
    fn event_stream(&self) -> impl std::future::Future<Output = Result<EventStream>> + Send;
}

/// Platform-specific media watcher implementation
pub enum PlatformMediaWatcher {
    #[cfg(target_os = "linux")]
    Linux(crate::platform::linux::LinuxMediaWatcher),
    #[cfg(target_os = "macos")]
    MacOS(crate::platform::macos::MacOSMediaWatcher),
    #[cfg(target_os = "windows")]
    Windows(crate::platform::windows::WindowsMediaWatcher),
}

impl MediaWatcher for PlatformMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(w) => w.list_players().await,
            #[cfg(target_os = "macos")]
            Self::MacOS(w) => w.list_players().await,
            #[cfg(target_os = "windows")]
            Self::Windows(w) => w.list_players().await,
        }
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(w) => w.get_player(player_name).await,
            #[cfg(target_os = "macos")]
            Self::MacOS(w) => w.get_player(player_name).await,
            #[cfg(target_os = "windows")]
            Self::Windows(w) => w.get_player(player_name).await,
        }
    }

    async fn event_stream(&self) -> Result<EventStream> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(w) => w.event_stream().await,
            #[cfg(target_os = "macos")]
            Self::MacOS(w) => w.event_stream().await,
            #[cfg(target_os = "windows")]
            Self::Windows(w) => w.event_stream().await,
        }
    }
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
    pub async fn build(self) -> Result<PlatformMediaWatcher> {
        #[cfg(target_os = "linux")]
        {
            Ok(PlatformMediaWatcher::Linux(
                crate::platform::linux::LinuxMediaWatcher::new().await?,
            ))
        }

        #[cfg(target_os = "macos")]
        {
            Ok(PlatformMediaWatcher::MacOS(
                crate::platform::macos::MacOSMediaWatcher::new().await?,
            ))
        }

        #[cfg(target_os = "windows")]
        {
            Ok(PlatformMediaWatcher::Windows(
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
