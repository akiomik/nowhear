//! Linux platform implementation using MPRIS D-Bus interface.
//!
//! This module provides media player integration for Linux systems through the
//! [MPRIS (Media Player Remote Interfacing Specification)](https://specifications.freedesktop.org/mpris-spec/latest/)
//! D-Bus interface.

mod metadata;
mod playback_status;
mod provider;

pub use metadata::MprisMetadata;
pub use playback_status::MprisPlaybackStatus;
pub use provider::{MprisProvider, PlayerDiscoveryProvider};

use std::sync::Arc;

use crate::error::Result;
use crate::source::{EventStream, MediaSource};
use crate::types::PlayerInfo;

/// Linux media source implementation using MPRIS D-Bus interface.
///
/// This implementation uses the [MPRIS (Media Player Remote Interfacing Specification)](https://specifications.freedesktop.org/mpris-spec/latest/)
/// D-Bus interface to discover and interact with media players on Linux systems.
/// It supports any media player that implements the MPRIS interface, including:
///
/// - Spotify
/// - VLC
/// - Rhythmbox
/// - Audacious
/// - And many more
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`nowhear::MediaSourceBuilder`] to create media sources, which will
/// automatically select this implementation on Linux systems.
pub struct LinuxMediaSource<P: PlayerDiscoveryProvider = MprisProvider> {
    provider: Arc<P>,
}

impl LinuxMediaSource<MprisProvider> {
    /// Creates a new Linux media source.
    ///
    /// Note: This is an internal API. Use `MediaSourceBuilder` instead.
    pub async fn new() -> Result<Self> {
        Ok(Self {
            provider: Arc::new(MprisProvider::new().await?),
        })
    }
}

impl<P: PlayerDiscoveryProvider + 'static> LinuxMediaSource<P> {
    #[cfg(test)]
    pub const fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

impl<P: PlayerDiscoveryProvider + 'static> MediaSource for LinuxMediaSource<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.provider.discover_players().await
    }

    async fn get_player(&self, player_name: impl AsRef<str> + Send) -> Result<PlayerInfo> {
        self.provider.get_player_info(player_name.as_ref()).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.provider.create_event_stream();
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{collections::HashMap, time::Duration};

    use futures::{Stream, stream};

    use crate::{MediaEvent, MediaSourceError, PlaybackState, Track};

    /// Mock player discovery provider for testing.
    struct MockPlayerDiscoveryProvider {
        players: HashMap<String, PlayerInfo>,
    }

    impl MockPlayerDiscoveryProvider {
        fn new() -> Self {
            Self {
                players: HashMap::new(),
            }
        }

        fn with_player(mut self, player_name: &str, info: PlayerInfo) -> Self {
            self.players.insert(player_name.to_string(), info);
            self
        }
    }

    impl PlayerDiscoveryProvider for MockPlayerDiscoveryProvider {
        async fn discover_players(&self) -> Result<Vec<String>> {
            Ok(self.players.keys().cloned().collect())
        }

        async fn get_player_info(&self, player_name: &str) -> Result<PlayerInfo> {
            self.players
                .get(player_name)
                .cloned()
                .ok_or_else(|| MediaSourceError::PlayerNotFound(player_name.to_string()))
        }

        fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
            stream::empty()
        }
    }

    fn create_test_player_info(
        player_name: &str,
        track: Option<Track>,
        playback_state: PlaybackState,
        position: Option<Duration>,
        volume: Option<f64>,
    ) -> PlayerInfo {
        PlayerInfo {
            player_name: player_name.to_string(),
            current_track: track,
            playback_state,
            position,
            volume,
        }
    }

    fn create_test_track_for_linux(title: &str) -> Track {
        Track {
            title: title.to_string(),
            artist: vec!["Test Artist".to_string()],
            album: Some("Test Album".to_string()),
            album_artist: vec![],
            track_number: None,
            duration: Some(Duration::from_mins(3)),
            art_url: None,
        }
    }

    // LinuxMediaSource tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_players() -> Result<()> {
        let provider = Arc::new(MockPlayerDiscoveryProvider::new());
        let source = LinuxMediaSource::with_provider(provider);

        let players = source.list_players().await?;
        assert_eq!(players, Vec::<String>::new());

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_single_player() -> Result<()> {
        let info = create_test_player_info(
            "spotify",
            Some(create_test_track_for_linux("Test Song")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let provider = Arc::new(MockPlayerDiscoveryProvider::new().with_player("spotify", info));
        let source = LinuxMediaSource::with_provider(provider);

        let players = source.list_players().await?;
        assert_eq!(players, vec!["spotify".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_multiple_players() -> Result<()> {
        let spotify_info = create_test_player_info(
            "spotify",
            Some(create_test_track_for_linux("Spotify Song")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let vlc_info = create_test_player_info(
            "vlc",
            Some(create_test_track_for_linux("VLC Song")),
            PlaybackState::Paused,
            Some(Duration::from_secs(30)),
            Some(0.5),
        );
        let provider = Arc::new(
            MockPlayerDiscoveryProvider::new()
                .with_player("spotify", spotify_info)
                .with_player("vlc", vlc_info),
        );
        let source = LinuxMediaSource::with_provider(provider);

        let mut players = source.list_players().await?;
        players.sort();
        assert_eq!(players, vec!["spotify".to_string(), "vlc".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_with_active_player() -> Result<()> {
        let track = create_test_track_for_linux("Test Song");
        let info = create_test_player_info(
            "spotify",
            Some(track.clone()),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let provider = Arc::new(MockPlayerDiscoveryProvider::new().with_player("spotify", info));
        let source = LinuxMediaSource::with_provider(provider);

        let player_info = source.get_player("spotify").await?;
        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "spotify".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(10)),
                volume: Some(0.8),
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_not_found() {
        let provider = Arc::new(MockPlayerDiscoveryProvider::new());
        let source = LinuxMediaSource::with_provider(provider);

        let result = source.get_player("nonexistent").await;
        assert_eq!(
            result,
            Err(MediaSourceError::PlayerNotFound("nonexistent".to_string()))
        );
    }

    #[tokio::test]
    async fn test_get_player_paused_state() -> Result<()> {
        let track = create_test_track_for_linux("Paused Song");
        let info = create_test_player_info(
            "vlc",
            Some(track.clone()),
            PlaybackState::Paused,
            Some(Duration::from_secs(45)),
            Some(0.6),
        );
        let provider = Arc::new(MockPlayerDiscoveryProvider::new().with_player("vlc", info));
        let source = LinuxMediaSource::with_provider(provider);

        let player_info = source.get_player("vlc").await?;
        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "vlc".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs(45)),
                volume: Some(0.6),
            }
        );

        Ok(())
    }
}
