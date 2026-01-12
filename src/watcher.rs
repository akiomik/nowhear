use crate::error::Result;
use crate::types::{MediaEvent, PlayerInfo};
use futures::stream::BoxStream;

/// Type alias for event stream returned by the media watcher.
pub type EventStream = BoxStream<'static, MediaEvent>;

/// Main trait for media watching functionality.
///
/// This trait provides methods to list players, query player information,
/// and subscribe to media events. It is implemented by platform-specific
/// media watchers.
pub trait MediaWatcher: Send + Sync {
    /// Lists all currently available media players.
    ///
    /// # Returns
    ///
    /// Returns a vector of player names that are currently running and accessible.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use nowhear::{MediaWatcher, MediaWatcherBuilder};
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let watcher = MediaWatcherBuilder::new().build().await?;
    /// let players = watcher.list_players().await?;
    /// println!("Available players: {:?}", players);
    /// # Ok(())
    /// # }
    /// ```
    fn list_players(&self) -> impl std::future::Future<Output = Result<Vec<String>>> + Send;

    /// Gets detailed information about a specific player.
    ///
    /// # Arguments
    ///
    /// * `player_name` - The name of the player to query
    ///
    /// # Returns
    ///
    /// Returns `PlayerInfo` containing the current track, playback state, and other details.
    ///
    /// # Errors
    ///
    /// Returns `MediaWatcherError::PlayerNotFound` if the player is not running.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use nowhear::{MediaWatcher, MediaWatcherBuilder};
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let watcher = MediaWatcherBuilder::new().build().await?;
    /// let player_info = watcher.get_player("spotify").await?;
    /// println!("Current track: {:?}", player_info.current_track);
    /// # Ok(())
    /// # }
    /// ```
    fn get_player(
        &self,
        player_name: &str,
    ) -> impl std::future::Future<Output = Result<PlayerInfo>> + Send;

    /// Creates an event stream that emits media events.
    ///
    /// The stream yields events such as track changes, playback state changes,
    /// and player additions/removals. The stream continues indefinitely until dropped.
    ///
    /// # Returns
    ///
    /// Returns a stream that yields `MediaEvent` items.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use nowhear::{MediaWatcher, MediaWatcherBuilder};
    /// # use futures::StreamExt;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let watcher = MediaWatcherBuilder::new().build().await?;
    /// let mut stream = watcher.event_stream().await?;
    /// while let Some(event) = stream.next().await {
    ///     println!("Event: {:?}", event);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn event_stream(&self) -> impl std::future::Future<Output = Result<EventStream>> + Send;
}

/// Platform-specific media watcher implementation.
///
/// This enum wraps the appropriate platform-specific implementation
/// based on the target operating system. Users typically don't need
/// to interact with this type directly; use `MediaWatcherBuilder` instead.
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

/// Builder for creating a `MediaWatcher` instance.
///
/// This builder provides a convenient way to create a media watcher
/// for the current platform.
///
/// # Examples
///
/// ```no_run
/// # use nowhear::MediaWatcherBuilder;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let watcher = MediaWatcherBuilder::new().build().await?;
/// # Ok(())
/// # }
/// ```
pub struct MediaWatcherBuilder {
    // Future extensions: filter by player name, etc.
}

impl MediaWatcherBuilder {
    /// Creates a new builder instance.
    pub fn new() -> Self {
        Self {}
    }

    /// Builds and initializes the platform-specific media watcher.
    ///
    /// This method detects the current platform and creates the appropriate
    /// implementation (Linux, macOS, or Windows).
    ///
    /// # Returns
    ///
    /// Returns a `PlatformMediaWatcher` instance ready to use.
    ///
    /// # Errors
    ///
    /// Returns `MediaWatcherError::UnsupportedPlatform` if the current platform
    /// is not supported, or `MediaWatcherError::ConnectionError` if the platform-specific
    /// initialization fails.
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
