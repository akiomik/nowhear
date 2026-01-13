//! Windows-specific implementation using Windows Media Control API.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::future;
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;
use windows::Foundation::IStringable;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as WinSession,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as WinPlaybackStatus,
};
use windows::core::Interface;

use crate::error::{MediaSourceError, Result};
use crate::source::{EventStream, MediaSource};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};

/// Internal player state representation for Windows implementation.
///
/// This structure is used internally to track player state changes and
/// is not part of the public API.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub track: Track,
    pub playback_state: PlaybackState,
    pub position: Option<Duration>,
    pub volume: Option<f64>,
}

/// Internal trait for abstracting media session access.
///
/// This trait is used internally by the Windows implementation to allow
/// for dependency injection in tests. It is not part of the public API.
pub trait MediaSessionProvider: Send + Sync {
    fn get_all_sessions(&self)
    -> impl Future<Output = Result<HashMap<String, PlayerState>>> + Send;
    fn get_session_info(&self, session_id: &str)
    -> impl Future<Output = Result<PlayerInfo>> + Send;
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static;
}

/// Windows Media Control provider.
///
/// This provider uses the Windows Media Control API to query media sessions.
pub struct WindowsMediaControlProvider {
    manager: SessionManager,
}

impl WindowsMediaControlProvider {
    pub async fn new() -> Result<Self> {
        let manager = SessionManager::RequestAsync()
            .map_err(|e| {
                MediaSourceError::ConnectionError(format!("Failed to get session manager: {e}"))
            })?
            .await
            .map_err(|e| {
                MediaSourceError::ConnectionError(format!("Failed to await session manager: {e}"))
            })?;

        Ok(Self { manager })
    }

    fn get_sessions(&self) -> Result<Vec<WinSession>> {
        let sessions = self.manager.GetSessions().map_err(|e| {
            MediaSourceError::ConnectionError(format!("Failed to get sessions: {e}"))
        })?;

        let mut result = Vec::new();
        let size = sessions.Size().map_err(|e| {
            MediaSourceError::ConnectionError(format!("Failed to get sessions size: {e}"))
        })?;

        for i in 0..size {
            if let Ok(session) = sessions.GetAt(i) {
                result.push(session);
            }
        }

        Ok(result)
    }

    fn get_session_id(session: &WinSession) -> Result<String> {
        let app_id = session
            .SourceAppUserModelId()
            .map_err(|e| MediaSourceError::ParseError(format!("Failed to get app ID: {e}")))?;

        Ok(app_id.to_string())
    }

    #[allow(clippy::cast_sign_loss)]
    async fn get_session_state(session: &WinSession) -> Result<PlayerState> {
        let media_props = session
            .TryGetMediaPropertiesAsync()
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to get media properties: {e}"))
            })?
            .await
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to await media properties: {e}"))
            })?;

        let playback_info = session.GetPlaybackInfo().map_err(|e| {
            MediaSourceError::ParseError(format!("Failed to get playback info: {e}"))
        })?;

        let playback_status = playback_info.PlaybackStatus().map_err(|e| {
            MediaSourceError::ParseError(format!("Failed to get playback status: {e}"))
        })?;

        let timeline = session
            .GetTimelineProperties()
            .map_err(|e| MediaSourceError::ParseError(format!("Failed to get timeline: {e}")))?;

        let title = media_props.Title().unwrap_or_default().to_string();

        let artist_hstring = media_props.Artist().unwrap_or_default();
        let artist = if artist_hstring.is_empty() {
            vec![]
        } else {
            vec![artist_hstring.to_string()]
        };

        let album_title = media_props.AlbumTitle().ok();
        let album = album_title.map(|s| s.to_string()).filter(|s| !s.is_empty());

        let track_number = media_props.TrackNumber().ok().map(|number| number as u32);

        let art_url = media_props
            .Thumbnail()
            .ok()
            .and_then(|thumb| {
                thumb
                    .cast::<IStringable>()
                    .ok()
                    .and_then(|stringable| stringable.ToString().ok())
                    .map(|s| s.to_string())
            })
            .filter(|s: &String| !s.is_empty());

        let position_ticks = timeline.Position().ok();
        let position =
            position_ticks.map(|ticks| Duration::from_nanos((ticks.Duration as u64) * 100));

        let end_time_ticks = timeline.EndTime().ok();
        let duration =
            end_time_ticks.map(|ticks| Duration::from_nanos((ticks.Duration as u64) * 100));

        let track = Track {
            title: if title.is_empty() {
                "Unknown".to_string()
            } else {
                title
            },
            artist,
            album,
            album_artist: None,
            track_number,
            duration,
            art_url,
        };

        Ok(PlayerState {
            track,
            playback_state: parse_playback_status(playback_status),
            position,
            volume: None,
        })
    }
}

impl MediaSessionProvider for WindowsMediaControlProvider {
    async fn get_all_sessions(&self) -> Result<HashMap<String, PlayerState>> {
        let sessions = self.get_sessions()?;
        let mut session_states = HashMap::new();

        for session in sessions {
            if let Ok(session_id) = Self::get_session_id(&session)
                && let Ok(state) = Self::get_session_state(&session).await
            {
                session_states.insert(session_id, state);
            }
        }

        Ok(session_states)
    }

    async fn get_session_info(&self, session_id: &str) -> Result<PlayerInfo> {
        let sessions = self.get_sessions()?;

        for session in sessions {
            let id = Self::get_session_id(&session)?;
            if id == session_id {
                let state = Self::get_session_state(&session).await?;
                return Ok(PlayerInfo {
                    player_name: session_id.to_string(),
                    current_track: Some(state.track),
                    playback_state: state.playback_state,
                    position: state.position,
                    volume: state.volume,
                });
            }
        }

        Err(MediaSourceError::PlayerNotFound(session_id.to_string()))
    }

    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
        let manager = self.manager.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut monitor = PlayerMonitor::new();
            let mut poll_interval = interval(Duration::from_millis(1000));

            loop {
                poll_interval.tick().await;

                let sessions_vec = {
                    let sessions_result = manager.GetSessions();
                    sessions_result.map_or_else(
                        |_| None,
                        |sessions| {
                            let size_result = sessions.Size();
                            let mut vec = Vec::new();
                            if let Ok(size) = size_result {
                                for i in 0..size {
                                    if let Ok(session) = sessions.GetAt(i) {
                                        vec.push(session);
                                    }
                                }
                            }
                            Some(vec)
                        },
                    )
                };

                let Some(sessions_vec) = sessions_vec else {
                    sleep(Duration::from_secs(1)).await;
                    continue;
                };

                let mut current_states = HashMap::new();
                let mut futures = Vec::new();
                let mut session_ids = Vec::new();

                for session in &sessions_vec {
                    if let Ok(session_id) = Self::get_session_id(session) {
                        session_ids.push(session_id);
                        futures.push(Self::get_session_state(session));
                    }
                }

                let results = future::join_all(futures).await;

                for (session_id, result) in session_ids.into_iter().zip(results) {
                    if let Ok(state) = result {
                        current_states.insert(session_id, state);
                    }
                }

                let events = monitor.process_sessions(current_states);

                for event in events {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        });

        UnboundedReceiverStream::new(rx)
    }
}

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
/// The implementation polls for changes every 1 second to detect state changes.
/// Windows does not provide reliable event notifications for all media state changes,
/// so polling is used to ensure consistent behavior.
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`nowhear::MediaSourceBuilder`] to create media sources, which will
/// automatically select this implementation on Windows systems.
pub struct WindowsMediaSource<P: MediaSessionProvider = WindowsMediaControlProvider> {
    provider: Arc<P>,
}

struct PlayerMonitor {
    players: HashMap<String, PlayerState>,
}

impl PlayerMonitor {
    fn new() -> Self {
        Self {
            players: HashMap::new(),
        }
    }

    fn process_sessions(
        &mut self,
        current_sessions: HashMap<String, PlayerState>,
    ) -> Vec<MediaEvent> {
        let mut events = Vec::new();

        // Check for new and changed players
        for (session_id, current_state) in &current_sessions {
            if let Some(last_state) = self.players.get(session_id) {
                // Existing player - detect changes
                Self::detect_changes(session_id, last_state, current_state, &mut events);
            } else {
                // New player
                events.push(MediaEvent::PlayerAdded {
                    player_name: session_id.clone(),
                });
                events.push(MediaEvent::TrackChanged {
                    player_name: session_id.clone(),
                    track: current_state.track.clone(),
                });
                events.push(MediaEvent::StateChanged {
                    player_name: session_id.clone(),
                    state: current_state.playback_state,
                });
            }
        }

        // Check for removed players
        let removed_players: Vec<String> = self
            .players
            .keys()
            .filter(|id| !current_sessions.contains_key(*id))
            .cloned()
            .collect();

        for player_id in removed_players {
            events.push(MediaEvent::PlayerRemoved {
                player_name: player_id.clone(),
            });
            self.players.remove(&player_id);
        }

        // Update stored states
        self.players = current_sessions;

        events
    }

    fn detect_changes(
        player_name: &str,
        last: &PlayerState,
        current: &PlayerState,
        events: &mut Vec<MediaEvent>,
    ) {
        // Check for track change
        if last.track != current.track {
            events.push(MediaEvent::TrackChanged {
                player_name: player_name.to_string(),
                track: current.track.clone(),
            });
        }

        // Check for playback state change
        if last.playback_state != current.playback_state {
            events.push(MediaEvent::StateChanged {
                player_name: player_name.to_string(),
                state: current.playback_state,
            });
        }

        // Check for position change (seek detection)
        if let Some(current_pos) = current.position {
            let should_emit = last.position.is_none_or(|last_pos| {
                // Detect seek: position difference > 2 seconds
                let diff = current_pos.abs_diff(last_pos);
                diff > Duration::from_secs(2)
            });

            if should_emit {
                events.push(MediaEvent::PositionChanged {
                    player_name: player_name.to_string(),
                    position: current_pos,
                });
            }
        }

        // Check for volume change
        if current.volume != last.volume
            && let Some(vol) = current.volume
        {
            events.push(MediaEvent::VolumeChanged {
                player_name: player_name.to_string(),
                volume: vol,
            });
        }
    }
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

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        self.provider.get_session_info(player_name).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.provider.create_event_stream();
        Ok(Box::pin(stream))
    }
}

const fn parse_playback_status(status: WinPlaybackStatus) -> PlaybackState {
    match status {
        WinPlaybackStatus::Playing => PlaybackState::Playing,
        WinPlaybackStatus::Paused => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

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
            album_artist: None,
            track_number: None,
            duration: Some(Duration::from_secs(180)),
            art_url: None,
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

    // Playback status parsing tests

    #[test]
    fn test_parse_playback_status_playing() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Playing),
            PlaybackState::Playing
        );
    }

    #[test]
    fn test_parse_playback_status_paused() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Paused),
            PlaybackState::Paused
        );
    }

    #[test]
    fn test_parse_playback_status_stopped() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Stopped),
            PlaybackState::Stopped
        );
    }

    #[test]
    fn test_parse_playback_status_closed() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Closed),
            PlaybackState::Stopped
        );
    }

    #[test]
    fn test_parse_playback_status_changing() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Changing),
            PlaybackState::Stopped
        );
    }

    #[test]
    fn test_player_monitor_new() {
        let monitor = PlayerMonitor::new();
        assert_eq!(monitor.players.len(), 0);
    }

    #[test]
    fn test_player_monitor_first_player_added() {
        let mut monitor = PlayerMonitor::new();
        let mut sessions = HashMap::new();

        let track = create_test_track_for_windows("Song 1");
        let state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        sessions.insert("Spotify.exe".to_string(), state);

        let events = monitor.process_sessions(sessions);

        // Should get: PlayerAdded, TrackChanged, StateChanged
        assert_eq!(
            events,
            vec![
                MediaEvent::PlayerAdded {
                    player_name: "Spotify.exe".to_string(),
                },
                MediaEvent::TrackChanged {
                    player_name: "Spotify.exe".to_string(),
                    track,
                },
                MediaEvent::StateChanged {
                    player_name: "Spotify.exe".to_string(),
                    state: PlaybackState::Playing,
                }
            ]
        );
    }

    #[test]
    fn test_player_monitor_track_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let track1 = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track1,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with different track
        let mut new_sessions = HashMap::new();
        let track2 = create_test_track_for_windows("Song 2");
        let new_state = create_test_state_for_windows(
            track2.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect track change
        assert_eq!(
            events,
            vec![MediaEvent::TrackChanged {
                player_name: "Spotify.exe".to_string(),
                track: track2
            }]
        );
    }

    #[test]
    fn test_player_monitor_playback_state_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with different playback state
        let mut new_sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let new_state = create_test_state_for_windows(
            track,
            PlaybackState::Paused,
            Some(Duration::from_secs(10)),
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect state change
        assert_eq!(
            events,
            vec![MediaEvent::StateChanged {
                player_name: "Spotify.exe".to_string(),
                state: PlaybackState::Paused
            }]
        );
    }

    #[test]
    fn test_player_monitor_position_seek() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with significant position jump
        let mut new_sessions = HashMap::new();
        let new_state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(60)),
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect position change
        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "Spotify.exe".to_string(),
                position: Duration::from_secs(60),
            }]
        );
    }

    #[test]
    fn test_player_monitor_position_normal_playback() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with normal 1 second progression
        let mut new_sessions = HashMap::new();
        let new_state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should not detect position change for normal playback
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_player_monitor_player_removed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state - player running
        let mut initial_sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // Player stopped - empty sessions
        let empty_sessions = HashMap::new();
        let events = monitor.process_sessions(empty_sessions);

        // Should detect player removal
        assert_eq!(
            events,
            vec![MediaEvent::PlayerRemoved {
                player_name: "Spotify.exe".to_string()
            }]
        );

        // State should be cleared
        assert!(!monitor.players.contains_key("Spotify.exe"));
    }

    #[test]
    fn test_player_monitor_multiple_players() {
        let mut monitor = PlayerMonitor::new();
        let mut sessions = HashMap::new();

        // Add Spotify
        let track1 = create_test_track_for_windows("Song 1");
        let spotify_state = create_test_state_for_windows(
            track1,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        sessions.insert("Spotify.exe".to_string(), spotify_state);

        // Add VLC
        let track2 = create_test_track_for_windows("Song 2");
        let vlc_state = create_test_state_for_windows(
            track2,
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
        );
        sessions.insert("vlc.exe".to_string(), vlc_state);

        let events = monitor.process_sessions(sessions);

        // Should get events for both players
        assert!(events.len() >= 6); // 2 players * (Added + Track + State)

        // Both should be tracked
        assert_eq!(monitor.players.len(), 2);
        assert!(monitor.players.contains_key("Spotify.exe"));
        assert!(monitor.players.contains_key("vlc.exe"));
    }

    #[test]
    fn test_player_monitor_multiple_changes() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let track1 = create_test_track_for_windows("Song 1");
        let initial_state = create_test_state_for_windows(
            track1,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with multiple changes
        let mut new_sessions = HashMap::new();
        let track2 = create_test_track_for_windows("Song 2");
        let new_state = create_test_state_for_windows(
            track2.clone(),
            PlaybackState::Paused,
            Some(Duration::from_secs(60)),
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect all changes: track, state, position
        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "Spotify.exe".to_string(),
                    track: track2,
                },
                MediaEvent::StateChanged {
                    player_name: "Spotify.exe".to_string(),
                    state: PlaybackState::Paused,
                },
                MediaEvent::PositionChanged {
                    player_name: "Spotify.exe".to_string(),
                    position: Duration::from_secs(60),
                },
            ]
        );
    }

    #[test]
    fn test_player_monitor_no_changes() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut sessions = HashMap::new();
        let track = create_test_track_for_windows("Song 1");
        let state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        sessions.insert("Spotify.exe".to_string(), state);
        monitor.process_sessions(sessions.clone());

        // Same state again
        let events = monitor.process_sessions(sessions);

        // Should not generate any events
        assert_eq!(events, Vec::<MediaEvent>::new());
    }
}
