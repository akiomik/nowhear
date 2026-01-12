//! Windows-specific implementation using Windows Media Control API.

#[cfg(target_os = "windows")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use futures::stream::Stream;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as WinSession,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as WinPlaybackStatus,
};

/// Internal player state representation for Windows implementation.
///
/// This structure is used internally to track player state changes and
/// is not part of the public API.
#[derive(Debug, Clone, PartialEq)]
#[doc(hidden)]
pub struct PlayerState {
    pub(crate) track: Track,
    pub(crate) playback_state: PlaybackState,
    pub(crate) position: Option<Duration>,
    pub(crate) volume: Option<f64>,
}

/// Internal trait for abstracting media session access.
///
/// This trait is used internally by the Windows implementation to allow
/// for dependency injection in tests. It is not part of the public API.
#[doc(hidden)]
pub trait MediaSessionProvider: Send + Sync {
    fn get_all_sessions(
        &self,
    ) -> impl std::future::Future<Output = Result<HashMap<String, PlayerState>>> + Send;
    fn get_session_info(
        &self,
        session_id: &str,
    ) -> impl std::future::Future<Output = Result<PlayerInfo>> + Send;
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send;
}

/// Windows Media Control provider.
///
/// This provider uses the Windows Media Control API to query media sessions.
#[doc(hidden)]
pub struct WindowsMediaControlProvider {
    manager: SessionManager,
}

impl WindowsMediaControlProvider {
    #[doc(hidden)]
    pub async fn new() -> Result<Self> {
        let manager = SessionManager::RequestAsync()
            .map_err(|e| {
                MediaWatcherError::ConnectionError(format!("Failed to get session manager: {}", e))
            })?
            .await
            .map_err(|e| {
                MediaWatcherError::ConnectionError(format!(
                    "Failed to await session manager: {}",
                    e
                ))
            })?;

        Ok(Self { manager })
    }

    async fn get_sessions(&self) -> Result<Vec<WinSession>> {
        let sessions = self.manager.GetSessions().map_err(|e| {
            MediaWatcherError::ConnectionError(format!("Failed to get sessions: {}", e))
        })?;

        Ok(sessions.into_iter().collect())
    }

    fn get_session_id(session: &WinSession) -> Result<String> {
        let app_id = session
            .SourceAppUserModelId()
            .map_err(|e| MediaWatcherError::ParseError(format!("Failed to get app ID: {}", e)))?;

        Ok(app_id.to_string())
    }

    async fn get_session_state(session: &WinSession) -> Result<PlayerState> {
        let media_props = session
            .TryGetMediaPropertiesAsync()
            .map_err(|e| {
                MediaWatcherError::ParseError(format!("Failed to get media properties: {}", e))
            })?
            .await
            .map_err(|e| {
                MediaWatcherError::ParseError(format!("Failed to await media properties: {}", e))
            })?;

        let playback_info = session.GetPlaybackInfo().map_err(|e| {
            MediaWatcherError::ParseError(format!("Failed to get playback info: {}", e))
        })?;

        let playback_status = playback_info.PlaybackStatus().map_err(|e| {
            MediaWatcherError::ParseError(format!("Failed to get playback status: {}", e))
        })?;

        let timeline = session
            .GetTimelineProperties()
            .map_err(|e| MediaWatcherError::ParseError(format!("Failed to get timeline: {}", e)))?;

        let title = media_props.Title().unwrap_or_default().to_string();

        let artist_hstring = media_props.Artist().unwrap_or_default();
        let artist = if artist_hstring.is_empty() {
            vec![]
        } else {
            vec![artist_hstring.to_string()]
        };

        let album_title = media_props.AlbumTitle().ok();
        let album = album_title.map(|s| s.to_string()).filter(|s| !s.is_empty());

        let track_number = media_props.TrackNumber().ok();

        let art_url = media_props
            .Thumbnail()
            .ok()
            .and_then(|thumb| thumb.ToString().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

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
        let sessions = self.get_sessions().await?;
        let mut session_states = HashMap::new();

        for session in sessions {
            if let Ok(session_id) = Self::get_session_id(&session) {
                if let Ok(state) = Self::get_session_state(&session).await {
                    session_states.insert(session_id, state);
                }
            }
        }

        Ok(session_states)
    }

    async fn get_session_info(&self, session_id: &str) -> Result<PlayerInfo> {
        let sessions = self.get_sessions().await?;

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

        Err(MediaWatcherError::PlayerNotFound(session_id.to_string()))
    }

    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send {
        let manager = self.manager.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut monitor = PlayerMonitor::new();
            let mut poll_interval = interval(Duration::from_millis(1000));

            loop {
                poll_interval.tick().await;

                let sessions = match manager.GetSessions() {
                    Ok(sessions) => sessions,
                    Err(_) => {
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                let mut current_states = HashMap::new();
                let mut futures = Vec::new();
                let mut session_ids = Vec::new();

                for session in sessions {
                    if let Ok(session_id) = WindowsMediaControlProvider::get_session_id(&session) {
                        session_ids.push(session_id);
                        futures.push(WindowsMediaControlProvider::get_session_state(&session));
                    }
                }

                let results = futures::future::join_all(futures).await;

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

/// Windows media watcher implementation using Windows Media Control API.
///
/// Note: This type is visible for technical reasons but should not be used directly.
/// Use `MediaWatcherBuilder` to create media watchers.
pub struct WindowsMediaWatcher<P: MediaSessionProvider = WindowsMediaControlProvider> {
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
                self.detect_changes(session_id, last_state, current_state, &mut events);
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
        &self,
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
            let should_emit = if let Some(last_pos) = last.position {
                // Detect seek: position difference > 2 seconds
                let diff = current_pos.abs_diff(last_pos);
                diff > Duration::from_secs(2)
            } else {
                true // First position
            };

            if should_emit {
                events.push(MediaEvent::PositionChanged {
                    player_name: player_name.to_string(),
                    position: current_pos,
                });
            }
        }

        // Check for volume change
        if current.volume != last.volume {
            if let Some(vol) = current.volume {
                events.push(MediaEvent::VolumeChanged {
                    player_name: player_name.to_string(),
                    volume: vol,
                });
            }
        }
    }
}

impl WindowsMediaWatcher<WindowsMediaControlProvider> {
    /// Creates a new Windows media watcher.
    ///
    /// Note: This is an internal API. Use `MediaWatcherBuilder` instead.
    #[doc(hidden)]
    pub async fn new() -> Result<Self> {
        Ok(Self {
            provider: Arc::new(WindowsMediaControlProvider::new().await?),
        })
    }
}

impl<P: MediaSessionProvider + 'static> WindowsMediaWatcher<P> {
    #[cfg(test)]
    pub fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

impl<P: MediaSessionProvider + 'static> MediaWatcher for WindowsMediaWatcher<P> {
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

fn parse_playback_status(status: WinPlaybackStatus) -> PlaybackState {
    match status {
        WinPlaybackStatus::Playing => PlaybackState::Playing,
        WinPlaybackStatus::Paused => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                .ok_or_else(|| MediaWatcherError::PlayerNotFound(session_id.to_string()))
        }

        fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send {
            futures::stream::empty()
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
        title: &str,
        playback_state: PlaybackState,
        position: Option<Duration>,
    ) -> PlayerState {
        PlayerState {
            track: create_test_track_for_windows(title),
            playback_state,
            position,
            volume: None,
        }
    }

    // WindowsMediaWatcher tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_sessions() {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await.unwrap();
        assert_eq!(players.len(), 0);
    }

    #[tokio::test]
    async fn test_list_players_with_single_session() {
        let state = create_test_state_for_windows(
            "Test Song",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await.unwrap();
        assert_eq!(players.len(), 1);
        assert!(players.contains(&"Spotify.exe".to_string()));
    }

    #[tokio::test]
    async fn test_list_players_with_multiple_sessions() {
        let spotify_state = create_test_state_for_windows(
            "Spotify Song",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let vlc_state = create_test_state_for_windows(
            "VLC Song",
            PlaybackState::Paused,
            Some(Duration::from_secs(30)),
        );
        let provider = Arc::new(
            MockMediaSessionProvider::new()
                .with_session("Spotify.exe", spotify_state)
                .with_session("vlc.exe", vlc_state),
        );
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await.unwrap();
        assert_eq!(players.len(), 2);
        assert!(players.contains(&"Spotify.exe".to_string()));
        assert!(players.contains(&"vlc.exe".to_string()));
    }

    #[tokio::test]
    async fn test_get_player_with_active_session() {
        let state = create_test_state_for_windows(
            "Test Song",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("Spotify.exe").await.unwrap();
        assert_eq!(player_info.player_name, "Spotify.exe");
        assert!(player_info.current_track.is_some());
        assert_eq!(player_info.current_track.unwrap().title, "Test Song");
        assert_eq!(player_info.playback_state, PlaybackState::Playing);
        assert_eq!(player_info.position, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn test_get_player_not_found() {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let result = watcher.get_player("nonexistent.exe").await;
        assert!(result.is_err());
        if let Err(MediaWatcherError::PlayerNotFound(name)) = result {
            assert_eq!(name, "nonexistent.exe");
        } else {
            panic!("Expected PlayerNotFound error");
        }
    }

    #[tokio::test]
    async fn test_get_player_paused_state() {
        let state = create_test_state_for_windows(
            "Paused Song",
            PlaybackState::Paused,
            Some(Duration::from_secs(45)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("vlc.exe", state));
        let watcher = WindowsMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("vlc.exe").await.unwrap();
        assert_eq!(player_info.player_name, "vlc.exe");
        assert_eq!(player_info.playback_state, PlaybackState::Paused);
        assert_eq!(player_info.position, Some(Duration::from_secs(45)));
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

    // PlayerMonitor tests
    fn create_test_player_state(
        title: &str,
        playback_state: PlaybackState,
        position: Option<Duration>,
        volume: Option<f64>,
    ) -> PlayerState {
        PlayerState {
            track: Track {
                title: title.to_string(),
                artist: vec!["Test Artist".to_string()],
                album: Some("Test Album".to_string()),
                album_artist: None,
                track_number: None,
                duration: Some(Duration::from_secs(180)),
                art_url: None,
            },
            playback_state,
            position,
            volume,
        }
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

        let state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        sessions.insert("Spotify.exe".to_string(), state);

        let events = monitor.process_sessions(sessions);

        // Should get: PlayerAdded, TrackChanged, StateChanged
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], MediaEvent::PlayerAdded { .. }));
        assert!(matches!(events[1], MediaEvent::TrackChanged { .. }));
        assert!(matches!(events[2], MediaEvent::StateChanged { .. }));
    }

    #[test]
    fn test_player_monitor_track_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with different track
        let mut new_sessions = HashMap::new();
        let new_state = create_test_player_state(
            "Song 2",
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
            None,
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect track change
        assert_eq!(events.len(), 1);
        if let MediaEvent::TrackChanged { player_name, track } = &events[0] {
            assert_eq!(player_name, "Spotify.exe");
            assert_eq!(track.title, "Song 2");
        } else {
            panic!("Expected TrackChanged event");
        }
    }

    #[test]
    fn test_player_monitor_playback_state_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with different playback state
        let mut new_sessions = HashMap::new();
        let new_state = create_test_player_state(
            "Song 1",
            PlaybackState::Paused,
            Some(Duration::from_secs(10)),
            None,
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect state change
        assert_eq!(events.len(), 1);
        if let MediaEvent::StateChanged { player_name, state } = &events[0] {
            assert_eq!(player_name, "Spotify.exe");
            assert_eq!(*state, PlaybackState::Paused);
        } else {
            panic!("Expected StateChanged event");
        }
    }

    #[test]
    fn test_player_monitor_position_seek() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with significant position jump
        let mut new_sessions = HashMap::new();
        let new_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(60)),
            None,
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect position change
        assert_eq!(events.len(), 1);
        if let MediaEvent::PositionChanged {
            player_name,
            position,
        } = &events[0]
        {
            assert_eq!(player_name, "Spotify.exe");
            assert_eq!(*position, Duration::from_secs(60));
        } else {
            panic!("Expected PositionChanged event");
        }
    }

    #[test]
    fn test_player_monitor_position_normal_playback() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut initial_sessions = HashMap::new();
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with normal 1 second progression
        let mut new_sessions = HashMap::new();
        let new_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
            None,
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should not detect position change for normal playback
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_player_monitor_player_removed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state - player running
        let mut initial_sessions = HashMap::new();
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // Player stopped - empty sessions
        let empty_sessions = HashMap::new();
        let events = monitor.process_sessions(empty_sessions);

        // Should detect player removal
        assert_eq!(events.len(), 1);
        if let MediaEvent::PlayerRemoved { player_name } = &events[0] {
            assert_eq!(player_name, "Spotify.exe");
        } else {
            panic!("Expected PlayerRemoved event");
        }

        // State should be cleared
        assert!(!monitor.players.contains_key("Spotify.exe"));
    }

    #[test]
    fn test_player_monitor_multiple_players() {
        let mut monitor = PlayerMonitor::new();
        let mut sessions = HashMap::new();

        // Add Spotify
        let spotify_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        sessions.insert("Spotify.exe".to_string(), spotify_state);

        // Add VLC
        let vlc_state = create_test_player_state(
            "Song 2",
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
            None,
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
        let initial_state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        initial_sessions.insert("Spotify.exe".to_string(), initial_state);
        monitor.process_sessions(initial_sessions);

        // New state with multiple changes
        let mut new_sessions = HashMap::new();
        let new_state = create_test_player_state(
            "Song 2",
            PlaybackState::Paused,
            Some(Duration::from_secs(60)),
            None,
        );
        new_sessions.insert("Spotify.exe".to_string(), new_state);
        let events = monitor.process_sessions(new_sessions);

        // Should detect all changes: track, state, position
        assert_eq!(events.len(), 3);

        let has_track_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::TrackChanged { .. }));
        let has_state_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::StateChanged { .. }));
        let has_position_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::PositionChanged { .. }));

        assert!(has_track_change);
        assert!(has_state_change);
        assert!(has_position_change);
    }

    #[test]
    fn test_player_monitor_no_changes() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let mut sessions = HashMap::new();
        let state = create_test_player_state(
            "Song 1",
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            None,
        );
        sessions.insert("Spotify.exe".to_string(), state.clone());
        monitor.process_sessions(sessions.clone());

        // Same state again
        let events = monitor.process_sessions(sessions);

        // Should not generate any events
        assert_eq!(events.len(), 0);
    }
}
