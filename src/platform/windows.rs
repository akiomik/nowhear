//! Windows-specific implementation using Windows Media Control API.

mod playback_status;
mod provider;
mod track;

use std::sync::Arc;

use crate::error::Result;
use crate::source::{EventStream, MediaSource};
use crate::types::PlayerInfo;

use provider::{MediaSessionProvider, WindowsMediaControlProvider};

/// Windows media source implementation using Windows Media Control API.
///
/// This implementation uses the Windows Runtime API
/// [`GlobalSystemMediaTransportControlsSessionManager`](https://learn.microsoft.com/en-us/uwp/api/windows.media.control.globalsystemmediatransportcontrolssessionmanager)
/// to interact with media players on Windows 10 and later.
///
/// It supports any application that integrates with Windows Media Control, including:
///
/// - Spotify
/// - VLC
/// - Windows Media Player
/// - Microsoft Edge (for web-based media)
/// - Chrome (for web-based media)
/// - And many more
///
/// # Implementation Details
///
/// The implementation is fully event-driven using Windows Runtime event handlers:
/// - `SessionsChanged`: Detects new or removed media players
/// - `MediaPropertiesChanged`: Detects track changes
/// - `PlaybackInfoChanged`: Detects playback state changes (play/pause/stop)
/// - `TimelinePropertiesChanged`: Detects position changes (seeking)
///
/// This provides real-time updates with minimal resource usage and no polling overhead.
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`crate::source::MediaSourceBuilder`] to create media sources, which will
/// automatically select this implementation on Windows systems.
pub struct WindowsMediaSource<P: MediaSessionProvider = WindowsMediaControlProvider> {
    provider: Arc<P>,
}

impl WindowsMediaSource<WindowsMediaControlProvider> {
    /// Creates a new Windows media source.
    ///
    /// Note: This is an internal API. Use `MediaSourceBuilder` instead.
    pub async fn new() -> Result<Self> {
        Ok(Self {
            provider: Arc::new(WindowsMediaControlProvider::new().await?),
        })
    }
}

impl<P: MediaSessionProvider + 'static> WindowsMediaSource<P> {
    #[cfg(test)]
    pub const fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

impl<P: MediaSessionProvider + 'static> MediaSource for WindowsMediaSource<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        let sessions = self.provider.get_all_sessions().await?;
        Ok(sessions.keys().cloned().collect())
    }

    async fn get_player(&self, player_name: impl AsRef<str> + Send) -> Result<PlayerInfo> {
        self.provider.get_session_info(player_name.as_ref()).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.provider.create_event_stream();
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::platform::state::PlayerState;

    use std::collections::HashMap;
    use std::time::Duration;

    use futures::{Stream, stream};

    use crate::error::MediaSourceError;
    use crate::types::{MediaEvent, PlaybackState, Track};

    /// Mock media session provider for testing.
    struct MockMediaSessionProvider {
        sessions: HashMap<String, PlayerState>,
    }

    impl MockMediaSessionProvider {
        fn new() -> Self {
            Self {
                sessions: HashMap::new(),
            }
        }

        fn with_session(mut self, session_id: &str, state: PlayerState) -> Self {
            self.sessions.insert(session_id.to_string(), state);
            self
        }
    }

    impl MediaSessionProvider for MockMediaSessionProvider {
        async fn get_all_sessions(&self) -> Result<HashMap<String, PlayerState>> {
            Ok(self.sessions.clone())
        }

        async fn get_session_info(&self, session_id: &str) -> Result<PlayerInfo> {
            self.sessions
                .get(session_id)
                .map(|state| PlayerInfo {
                    player_name: session_id.to_string(),
                    current_track: Some(state.track.clone()),
                    playback_state: state.playback_state,
                    position: state.position,
                    volume: state.volume,
                })
                .ok_or_else(|| MediaSourceError::PlayerNotFound(session_id.to_string()))
        }

        fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
            stream::empty()
        }
    }

    fn create_test_track_for_windows(title: &str) -> Track {
        Track {
            title: title.to_string(),
            artist: vec!["Test Artist".to_string()],
            album: Some("Test Album".to_string()),
            album_artist: vec![],
            track_number: None,
            duration: Some(Duration::from_mins(3)),
            artwork: None,
        }
    }

    fn create_test_state_for_windows(
        track: Track,
        playback_state: PlaybackState,
        position: Option<Duration>,
    ) -> PlayerState {
        PlayerState {
            track,
            playback_state,
            position,
            volume: None,
        }
    }
    // WindowsMediaSource tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_sessions() -> Result<()> {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let source = WindowsMediaSource::with_provider(provider);

        let players = source.list_players().await?;

        assert_eq!(players, Vec::<String>::new());

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_single_session() -> Result<()> {
        let track = create_test_track_for_windows("Test Song");
        let state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let players = source.list_players().await?;

        assert_eq!(players, vec!["Spotify.exe".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_multiple_sessions() -> Result<()> {
        let spotify_track = create_test_track_for_windows("Spotify Song");
        let spotify_state = create_test_state_for_windows(
            spotify_track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let vlc_track = create_test_track_for_windows("VLC Song");
        let vlc_state = create_test_state_for_windows(
            vlc_track,
            PlaybackState::Paused,
            Some(Duration::from_secs(30)),
        );
        let provider = Arc::new(
            MockMediaSessionProvider::new()
                .with_session("Spotify.exe", spotify_state)
                .with_session("vlc.exe", vlc_state),
        );
        let source = WindowsMediaSource::with_provider(provider);

        let mut players = source.list_players().await?;
        players.sort();

        assert_eq!(
            players,
            vec!["Spotify.exe".to_string(), "vlc.exe".to_string()]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_with_active_session() -> Result<()> {
        let track = create_test_track_for_windows("Test Song");
        let state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let player_info = source.get_player("Spotify.exe").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "Spotify.exe".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(10)),
                volume: None,
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_not_found() {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let source = WindowsMediaSource::with_provider(provider);

        let result = source.get_player("nonexistent.exe").await;
        assert_eq!(
            result,
            Err(MediaSourceError::PlayerNotFound(
                "nonexistent.exe".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn test_get_player_paused_state() -> Result<()> {
        let track = create_test_track_for_windows("Paused Song");
        let state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Paused,
            Some(Duration::from_secs(45)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("vlc.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let player_info = source.get_player("vlc.exe").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "vlc.exe".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs(45)),
                volume: None,
            }
        );

        Ok(())
    }
}
