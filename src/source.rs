//! Media source trait and builder.
//!
//! This module provides the core [`MediaSource`] trait that defines the interface
//! for interacting with media players, and the [`MediaSourceBuilder`] for creating
//! platform-specific implementations.

use std::future::Future;

use futures::stream::BoxStream;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use crate::error::MediaSourceError;
use crate::error::Result;
#[cfg(target_os = "linux")]
use crate::platform::linux::LinuxMediaSource;
#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSMediaSource;
#[cfg(target_os = "windows")]
use crate::platform::windows::WindowsMediaSource;
use crate::types::{MediaEvent, PlayerInfo};

/// Type alias for event stream returned by the media source.
///
/// This is a boxed stream that yields [`MediaEvent`] items. The stream is
/// `'static` and can be moved across thread boundaries.
///
/// # Examples
///
/// ```no_run
/// # use nowhear::{MediaSource, MediaSourceBuilder, Result};
/// # use futures::StreamExt;
/// # async fn example() -> Result<()> {
/// let source = MediaSourceBuilder::new().build().await?;
/// let mut stream = source.event_stream().await?;
///
/// // Stream will emit events indefinitely
/// while let Some(event) = stream.next().await {
///     println!("Received event: {:?}", event);
/// }
/// # Ok(())
/// # }
/// ```
pub type EventStream = BoxStream<'static, MediaEvent>;

/// Main trait for media source functionality.
///
/// This trait provides methods to list players, query player information,
/// and subscribe to media events. It is implemented by platform-specific
/// media sources.
pub trait MediaSource: Send + Sync {
    /// Lists all currently available media players.
    ///
    /// # Returns
    ///
    /// Returns a vector of player names that are currently running and accessible.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use nowhear::{MediaSource, MediaSourceBuilder, Result};
    /// # async fn example() -> Result<()> {
    /// let source = MediaSourceBuilder::new().build().await?;
    /// let players = source.list_players().await?;
    /// println!("Available players: {:?}", players);
    /// # Ok(())
    /// # }
    /// ```
    fn list_players(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

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
    /// Returns `MediaSourceError::PlayerNotFound` if the player is not running.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use nowhear::{MediaSource, MediaSourceBuilder, Result};
    /// # async fn example() -> Result<()> {
    /// let source = MediaSourceBuilder::new().build().await?;
    /// let player_info = source.get_player("spotify").await?;
    /// println!("Current track: {:?}", player_info.current_track);
    /// # Ok(())
    /// # }
    /// ```
    fn get_player(
        &self,
        player_name: impl AsRef<str> + Send,
    ) -> impl Future<Output = Result<PlayerInfo>> + Send;

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
    /// # use nowhear::{MediaSource, MediaSourceBuilder, Result};
    /// # use futures::StreamExt;
    /// # async fn example() -> Result<()> {
    /// let source = MediaSourceBuilder::new().build().await?;
    /// let mut stream = source.event_stream().await?;
    /// while let Some(event) = stream.next().await {
    ///     println!("Event: {:?}", event);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn event_stream(&self) -> impl Future<Output = Result<EventStream>> + Send;
}

enum PlatformMediaSourceInner {
    #[cfg(target_os = "linux")]
    Linux(LinuxMediaSource),
    #[cfg(target_os = "macos")]
    MacOS(MacOSMediaSource),
    #[cfg(target_os = "windows")]
    Windows(WindowsMediaSource),
}

/// Platform-specific media source implementation.
///
/// This struct wraps the appropriate platform-specific implementation based on the
/// target operating system. Users typically don't need to interact with this type
/// directly; use [`MediaSourceBuilder`] instead.
///
/// # Platform Implementations
///
/// - **Linux**: Uses MPRIS D-Bus interface to communicate with media players
/// - **macOS**: Uses AppleScript to query Music.app and Spotify
/// - **Windows**: Uses Windows Media Control API (`GlobalSystemMediaTransportControlsSessionManager`)
///
/// # Examples
///
/// ```no_run
/// use nowhear::{MediaSourceBuilder, Result};
///
/// # async fn example() -> Result<()> {
/// // The builder automatically selects the correct platform implementation
/// let source = MediaSourceBuilder::new().build().await?;
/// # Ok(())
/// # }
/// ```
pub struct PlatformMediaSource(PlatformMediaSourceInner);

impl MediaSource for PlatformMediaSource {
    async fn list_players(&self) -> Result<Vec<String>> {
        match &self.0 {
            #[cfg(target_os = "linux")]
            PlatformMediaSourceInner::Linux(w) => w.list_players().await,
            #[cfg(target_os = "macos")]
            PlatformMediaSourceInner::MacOS(w) => w.list_players().await,
            #[cfg(target_os = "windows")]
            PlatformMediaSourceInner::Windows(w) => w.list_players().await,
        }
    }

    async fn get_player(&self, player_name: impl AsRef<str> + Send) -> Result<PlayerInfo> {
        let player_name = player_name.as_ref();
        match &self.0 {
            #[cfg(target_os = "linux")]
            PlatformMediaSourceInner::Linux(w) => w.get_player(player_name).await,
            #[cfg(target_os = "macos")]
            PlatformMediaSourceInner::MacOS(w) => w.get_player(player_name).await,
            #[cfg(target_os = "windows")]
            PlatformMediaSourceInner::Windows(w) => w.get_player(player_name).await,
        }
    }

    async fn event_stream(&self) -> Result<EventStream> {
        match &self.0 {
            #[cfg(target_os = "linux")]
            PlatformMediaSourceInner::Linux(w) => w.event_stream().await,
            #[cfg(target_os = "macos")]
            PlatformMediaSourceInner::MacOS(w) => w.event_stream().await,
            #[cfg(target_os = "windows")]
            PlatformMediaSourceInner::Windows(w) => w.event_stream().await,
        }
    }
}

/// Builder for creating a `MediaSource` instance.
///
/// This builder provides a convenient way to create a media source
/// for the current platform.
///
/// # Examples
///
/// ```no_run
/// # use nowhear::{MediaSourceBuilder, Result};
/// # async fn example() -> Result<()> {
/// let source = MediaSourceBuilder::new().build().await?;
/// # Ok(())
/// # }
/// ```
pub struct MediaSourceBuilder {
    // Future extensions: filter by player name, etc.
}

impl MediaSourceBuilder {
    /// Creates a new builder instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use nowhear::MediaSourceBuilder;
    ///
    /// let builder = MediaSourceBuilder::new();
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }

    /// Builds and initializes the platform-specific media source.
    ///
    /// This method detects the current platform and creates the appropriate
    /// implementation (Linux, macOS, or Windows).
    ///
    /// # Returns
    ///
    /// Returns a `PlatformMediaSource` instance ready to use.
    ///
    /// # Errors
    ///
    /// Returns `MediaSourceError::UnsupportedPlatform` if the current platform
    /// is not supported, or `MediaSourceError::ConnectionError` if the platform-specific
    /// initialization fails.
    #[allow(clippy::unused_async)]
    pub async fn build(self) -> Result<PlatformMediaSource> {
        #[cfg(target_os = "linux")]
        {
            Ok(PlatformMediaSource(PlatformMediaSourceInner::Linux(
                LinuxMediaSource::new().await?,
            )))
        }

        #[cfg(target_os = "macos")]
        {
            Ok(PlatformMediaSource(PlatformMediaSourceInner::MacOS(
                MacOSMediaSource::new(),
            )))
        }

        #[cfg(target_os = "windows")]
        {
            Ok(PlatformMediaSource(PlatformMediaSourceInner::Windows(
                WindowsMediaSource::new().await?,
            )))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(MediaSourceError::UnsupportedPlatform)
        }
    }
}

impl Default for MediaSourceBuilder {
    /// Creates a new builder using the default configuration.
    ///
    /// This is equivalent to calling [`MediaSourceBuilder::new()`].
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_source_builder_new() {
        let _builder = MediaSourceBuilder::new();
    }

    #[test]
    fn test_media_source_builder_default() {
        let _builder = MediaSourceBuilder::default();
    }

    // On macOS, MacOSMediaSource::new() only wraps AppleScriptProvider in an Arc — no I/O.
    // Linux and Windows require a live session bus / WinRT, so they are excluded.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_media_source_builder_build_succeeds() {
        let result = MediaSourceBuilder::new().build().await;
        assert!(result.is_ok());
    }
}
