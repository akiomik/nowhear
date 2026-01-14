//! macOS-specific implementation using AppleScript.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::Stream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::error::{MediaSourceError, Result};
use crate::source::{EventStream, MediaSource};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};

/// Internal player state representation for macOS implementation.
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

/// Internal trait for abstracting player state retrieval.
///
/// This trait is used internally by the macOS implementation to allow
/// for dependency injection in tests. It is not part of the public API.
pub trait PlayerStateProvider: Send + Sync {
    fn get_player_state(
        &self,
        player_name: &str,
    ) -> impl Future<Output = Result<Option<PlayerState>>> + Send;
    fn list_available_players(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
}

/// AppleScript-based provider for macOS.
///
/// This provider uses AppleScript to query Music.app and Spotify for their
/// current playback state.
pub struct AppleScriptProvider;

impl PlayerStateProvider for AppleScriptProvider {
    async fn get_player_state(&self, player_name: &str) -> Result<Option<PlayerState>> {
        match player_name {
            "Music" => Self::get_music_app_state().await,
            "Spotify" => Self::get_spotify_state().await,
            _ => Err(MediaSourceError::PlayerNotFound(player_name.to_string())),
        }
    }

    async fn list_available_players(&self) -> Result<Vec<String>> {
        let mut players = Vec::new();

        if Self::get_music_app_state().await.ok().flatten().is_some() {
            players.push("Music".to_string());
        }

        if Self::get_spotify_state().await.ok().flatten().is_some() {
            players.push("Spotify".to_string());
        }

        Ok(players)
    }
}

impl AppleScriptProvider {
    async fn get_music_app_state() -> Result<Option<PlayerState>> {
        let script = include_str!("applescript/music.applescript");

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(AppleScriptFields::parse(&output).map(|f| f.to_music_player_state()))
        }
    }

    async fn get_spotify_state() -> Result<Option<PlayerState>> {
        let script = include_str!("applescript/spotify.applescript");

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(AppleScriptFields::parse(&output).map(|f| f.to_spotify_player_state()))
        }
    }
}

/// macOS media source implementation using AppleScript.
///
/// This implementation uses AppleScript to query the state of media players on macOS.
/// Currently supports:
///
/// - **Music.app** (formerly iTunes)
/// - **Spotify**
///
/// # Implementation Details
///
/// This implementation uses AppleScript for querying player state rather than
/// `NSDistributedNotificationCenter`, which would require running on the main thread.
/// The AppleScript approach with periodic polling (every 1 second) offers a simpler
/// alternative that works well in async contexts.
///
/// The polling interval is fixed at 1 second, which provides a good balance between
/// responsiveness and system resource usage.
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`nowhear::MediaSourceBuilder`] to create media sources, which will
/// automatically select this implementation on macOS systems.
pub struct MacOSMediaSource<P: PlayerStateProvider = AppleScriptProvider> {
    provider: Arc<P>,
}

struct PlayerMonitor {
    players: HashMap<String, PlayerState>,
    running: HashMap<String, bool>,
}

impl PlayerMonitor {
    fn new() -> Self {
        Self {
            players: HashMap::new(),
            running: HashMap::new(),
        }
    }

    fn process_player(
        &mut self,
        player_name: &str,
        current_state: Option<PlayerState>,
    ) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        let is_running = self.running.get(player_name).copied().unwrap_or(false);

        match current_state {
            Some(state) => {
                // Player is running
                if !is_running {
                    // Player just started
                    events.push(MediaEvent::PlayerAdded {
                        player_name: player_name.to_string(),
                    });
                    self.running.insert(player_name.to_string(), true);
                }

                // Check for state changes
                if let Some(last_state) = self.players.get(player_name) {
                    Self::detect_changes(player_name, last_state, &state, &mut events);
                } else {
                    // First time seeing this player with state
                    events.push(MediaEvent::TrackChanged {
                        player_name: player_name.to_string(),
                        track: state.track.clone(),
                    });
                    events.push(MediaEvent::StateChanged {
                        player_name: player_name.to_string(),
                        state: state.playback_state,
                    });
                }

                // Update stored state
                self.players.insert(player_name.to_string(), state);
            }
            None => {
                // Player is not running or not playing
                if is_running {
                    // Player was running but stopped
                    events.push(MediaEvent::PlayerRemoved {
                        player_name: player_name.to_string(),
                    });
                    self.running.insert(player_name.to_string(), false);
                    self.players.remove(player_name);
                }
            }
        }

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

impl MacOSMediaSource<AppleScriptProvider> {
    /// Creates a new macOS media source.
    ///
    /// Note: This is an internal API. Use `MediaSourceBuilder` instead.
    pub fn new() -> Self {
        Self {
            provider: Arc::new(AppleScriptProvider),
        }
    }
}

impl<P: PlayerStateProvider + 'static> MacOSMediaSource<P> {
    #[cfg(test)]
    pub const fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }

    fn create_event_stream_impl(provider: Arc<P>) -> impl Stream<Item = MediaEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut monitor = PlayerMonitor::new();
            let mut interval = time::interval(Duration::from_millis(1000));

            loop {
                interval.tick().await;

                // Poll both players in parallel
                let music_future = provider.get_player_state("Music");
                let spotify_future = provider.get_player_state("Spotify");
                let (music_state, spotify_state) = tokio::join!(music_future, spotify_future);

                // Process Music.app state
                let music_events = monitor.process_player("Music", music_state.ok().flatten());

                // Send Music.app events
                for event in music_events {
                    if tx.send(event).is_err() {
                        return; // Receiver dropped
                    }
                }

                // Process Spotify state
                let spotify_events =
                    monitor.process_player("Spotify", spotify_state.ok().flatten());

                // Send Spotify events
                for event in spotify_events {
                    if tx.send(event).is_err() {
                        return; // Receiver dropped
                    }
                }
            }
        });

        UnboundedReceiverStream::new(rx)
    }
}

impl<P: PlayerStateProvider + 'static> MediaSource for MacOSMediaSource<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.provider.list_available_players().await
    }

    async fn get_player(&self, player_name: impl AsRef<str> + Send) -> Result<PlayerInfo> {
        let player_name = player_name.as_ref();
        if let Some(player_state) = self.provider.get_player_state(player_name).await? {
            Ok(PlayerInfo {
                player_name: player_name.to_string(),
                current_track: Some(player_state.track),
                playback_state: player_state.playback_state,
                position: player_state.position,
                volume: player_state.volume,
            })
        } else {
            Ok(PlayerInfo::empty(player_name))
        }
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = Self::create_event_stream_impl(Arc::clone(&self.provider));
        Ok(Box::pin(stream))
    }
}

async fn execute_applescript(script: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await
        .map_err(|e| {
            MediaSourceError::InternalError(format!("Failed to execute AppleScript: {e}"))
        })?;

    if !output.status.success() {
        return Err(MediaSourceError::InternalError(format!(
            "AppleScript error: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Structured representation of AppleScript output fields.
///
/// This structure provides a clean abstraction over the tab-separated
/// output from AppleScript, making it easier to parse and test individual fields.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AppleScriptFields<'a> {
    parts: Vec<&'a str>,
}

impl<'a> AppleScriptFields<'a> {
    /// Parse tab-separated AppleScript output into structured fields.
    fn parse(output: &'a str) -> Option<Self> {
        let parts: Vec<&str> = output.split('\t').collect();
        if parts.len() >= 3 {
            Some(Self { parts })
        } else {
            None
        }
    }

    /// Get the track title.
    fn title(&self) -> &str {
        self.parts[0]
    }

    /// Get the track artist.
    fn artist(&self) -> &str {
        self.parts[1]
    }

    /// Get the album name, or None if empty.
    fn album(&self) -> Option<&str> {
        if self.parts[2].is_empty() {
            None
        } else {
            Some(self.parts[2])
        }
    }

    /// Get the album artist, or None if not available or empty.
    fn album_artist(&self) -> Option<&str> {
        self.parts.get(3).filter(|s| !s.trim().is_empty()).copied()
    }

    /// Get the track number, or None if not available or zero.
    fn track_number(&self) -> Option<u32> {
        self.parts
            .get(4)?
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
    }

    /// Get the playback state.
    fn playback_state(&self) -> PlaybackState {
        self.parts.get(5).map_or(PlaybackState::Playing, |s| {
            #[allow(clippy::match_same_arms)]
            match s.trim().to_lowercase().as_str() {
                "playing" | "fast forwarding" | "rewinding" => PlaybackState::Playing,
                "paused" => PlaybackState::Paused,
                "stopped" => PlaybackState::Stopped,
                _ => PlaybackState::Playing,
            }
        })
    }

    /// Get the playback position in seconds.
    fn position(&self) -> Option<Duration> {
        self.parts
            .get(6)?
            .trim()
            .parse::<f64>()
            .ok()
            .map(Duration::from_secs_f64)
    }

    /// Get the volume as a value between 0.0 and 1.0.
    fn volume(&self) -> Option<f64> {
        self.parts
            .get(7)?
            .trim()
            .parse::<f64>()
            .ok()
            .map(|vol| vol / 100.0)
    }

    /// Get the track duration in seconds (for Music.app).
    fn duration_in_seconds(&self) -> Option<Duration> {
        self.parts
            .get(8)?
            .trim()
            .parse::<f64>()
            .ok()
            .map(Duration::from_secs_f64)
    }

    /// Get the track duration in milliseconds (for Spotify).
    fn duration_in_millis(&self) -> Option<Duration> {
        self.parts
            .get(8)?
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis)
    }

    /// Convert fields to a `Track` struct for Music.app.
    fn to_music_track(&self) -> Track {
        Track {
            title: self.title().to_string(),
            artist: vec![self.artist().to_string()],
            album: self.album().map(String::from),
            album_artist: self.album_artist().map(|s| vec![s.to_string()]),
            track_number: self.track_number(),
            duration: self.duration_in_seconds(),
            art_url: None,
        }
    }

    /// Convert fields to a `Track` struct for Spotify.
    fn to_spotify_track(&self) -> Track {
        Track {
            title: self.title().to_string(),
            artist: vec![self.artist().to_string()],
            album: self.album().map(String::from),
            album_artist: self.album_artist().map(|s| vec![s.to_string()]),
            track_number: self.track_number(),
            duration: self.duration_in_millis(),
            art_url: None,
        }
    }

    /// Convert fields to a `PlayerState` struct for Music.app.
    fn to_music_player_state(&self) -> PlayerState {
        PlayerState {
            track: self.to_music_track(),
            playback_state: self.playback_state(),
            position: self.position(),
            volume: self.volume(),
        }
    }

    /// Convert fields to a `PlayerState` struct for Spotify.
    fn to_spotify_player_state(&self) -> PlayerState {
        PlayerState {
            track: self.to_spotify_track(),
            playback_state: self.playback_state(),
            position: self.position(),
            volume: self.volume(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock player state provider for testing.
    ///
    /// This provider returns predefined player states without executing
    /// any system commands, making tests fast and deterministic.
    struct MockPlayerStateProvider {
        states: HashMap<String, Option<PlayerState>>,
    }

    impl MockPlayerStateProvider {
        fn new() -> Self {
            Self {
                states: HashMap::new(),
            }
        }

        fn with_player(mut self, player_name: &str, state: Option<PlayerState>) -> Self {
            self.states.insert(player_name.to_string(), state);
            self
        }
    }

    impl PlayerStateProvider for MockPlayerStateProvider {
        async fn get_player_state(&self, player_name: &str) -> Result<Option<PlayerState>> {
            Ok(self.states.get(player_name).cloned().flatten())
        }

        async fn list_available_players(&self) -> Result<Vec<String>> {
            let players: Vec<String> = self
                .states
                .iter()
                .filter_map(|(name, state)| state.as_ref().map(|_| name.clone()))
                .collect();
            Ok(players)
        }
    }

    fn create_test_track(title: &str) -> Track {
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

    fn create_test_player_state_with_track(
        track: Track,
        playback_state: PlaybackState,
        position: Option<Duration>,
        volume: Option<f64>,
    ) -> PlayerState {
        PlayerState {
            track,
            playback_state,
            position,
            volume,
        }
    }

    // MacOSMediaSource tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_players() -> Result<()> {
        let provider = Arc::new(MockPlayerStateProvider::new());
        let source = MacOSMediaSource::with_provider(provider);

        let players = source.list_players().await?;
        assert_eq!(players, Vec::<String>::new());

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_music_only() -> Result<()> {
        let state = create_test_player_state_with_track(
            create_test_track("Test Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let provider = Arc::new(MockPlayerStateProvider::new().with_player("Music", Some(state)));
        let source = MacOSMediaSource::with_provider(provider);

        let players = source.list_players().await?;
        assert_eq!(players, vec!["Music".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_both_players() -> Result<()> {
        let music_state = create_test_player_state_with_track(
            create_test_track("Music Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let spotify_state = create_test_player_state_with_track(
            create_test_track("Spotify Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
            Some(0.7),
        );
        let provider = Arc::new(
            MockPlayerStateProvider::new()
                .with_player("Music", Some(music_state))
                .with_player("Spotify", Some(spotify_state)),
        );
        let source = MacOSMediaSource::with_provider(provider);

        let mut players = source.list_players().await?;
        players.sort();
        assert_eq!(players, vec!["Music".to_string(), "Spotify".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_with_active_player() -> Result<()> {
        let track = create_test_track("Test Song");
        let state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let provider = Arc::new(MockPlayerStateProvider::new().with_player("Music", Some(state)));
        let source = MacOSMediaSource::with_provider(provider);

        let player_info = source.get_player("Music").await?;
        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "Music".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(10)),
                volume: Some(0.8),
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_with_inactive_player() -> Result<()> {
        let provider = Arc::new(MockPlayerStateProvider::new().with_player("Music", None));
        let source = MacOSMediaSource::with_provider(provider);

        let player_info = source.get_player("Music").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "Music".to_string(),
                current_track: None,
                playback_state: PlaybackState::Stopped,
                position: None,
                volume: None,
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_paused_state() -> Result<()> {
        let track = create_test_track("Paused Song");
        let state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Paused,
            Some(Duration::from_secs(30)),
            Some(0.5),
        );
        let provider = Arc::new(MockPlayerStateProvider::new().with_player("Spotify", Some(state)));
        let source = MacOSMediaSource::with_provider(provider);

        let player_info = source.get_player("Spotify").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "Spotify".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs(30)),
                volume: Some(0.5),
            }
        );

        Ok(())
    }

    // AppleScriptFields tests

    #[test]
    fn test_parse_returns_none_for_insufficient_fields() {
        assert_eq!(AppleScriptFields::parse(""), None);
        assert_eq!(AppleScriptFields::parse("Title"), None);
        assert_eq!(AppleScriptFields::parse("Title\tArtist"), None);
    }

    #[test]
    fn test_parse_succeeds_with_minimum_fields() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum").expect("should parse");
        assert_eq!(fields.title(), "Title");
        assert_eq!(fields.artist(), "Artist");
        assert_eq!(fields.album(), Some("Album"));
    }

    #[test]
    fn test_title_and_artist_always_available() {
        let fields = AppleScriptFields::parse("My Song\tMy Artist\t").expect("should parse");
        assert_eq!(fields.title(), "My Song");
        assert_eq!(fields.artist(), "My Artist");
    }

    #[test]
    fn test_album_returns_none_when_empty() {
        let fields = AppleScriptFields::parse("Title\tArtist\t").expect("should parse");
        assert_eq!(fields.album(), None);
    }

    #[test]
    fn test_album_returns_some_when_present() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum Name").expect("should parse");
        assert_eq!(fields.album(), Some("Album Name"));
    }

    #[test]
    fn test_album_artist_returns_none_when_missing() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum").expect("should parse");
        assert_eq!(fields.album_artist(), None);
    }

    #[test]
    fn test_album_artist_returns_none_when_empty() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum\t").expect("should parse");
        assert_eq!(fields.album_artist(), None);
    }

    #[test]
    fn test_album_artist_returns_some_when_present() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\tAlbum Artist").expect("should parse");
        assert_eq!(fields.album_artist(), Some("Album Artist"));
    }

    #[test]
    fn test_track_number_returns_none_when_missing() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\tArtist").expect("should parse");
        assert_eq!(fields.track_number(), None);
    }

    #[test]
    fn test_track_number_returns_none_when_zero() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\tArtist\t0").expect("should parse");
        assert_eq!(fields.track_number(), None);
    }

    #[test]
    fn test_track_number_returns_some_when_valid() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\tArtist\t5").expect("should parse");
        assert_eq!(fields.track_number(), Some(5));
    }

    #[test]
    fn test_playback_state_defaults_to_playing() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Playing);
    }

    #[test]
    fn test_playback_state_playing() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\tplaying").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Playing);
    }

    #[test]
    fn test_playback_state_paused() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\tpaused").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Paused);
    }

    #[test]
    fn test_playback_state_stopped() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\tstopped").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Stopped);
    }

    #[test]
    fn test_playback_state_case_insensitive() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\tPLAYING").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Playing);
    }

    #[test]
    fn test_playback_state_unknown_defaults_to_playing() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\tunknown").expect("should parse");
        assert_eq!(fields.playback_state(), PlaybackState::Playing);
    }

    #[test]
    fn test_position_returns_none_when_missing() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum").expect("should parse");
        assert_eq!(fields.position(), None);
    }

    #[test]
    fn test_position_returns_some_when_present() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\t\t60.5").expect("should parse");
        assert_eq!(fields.position(), Some(Duration::from_secs_f64(60.5)));
    }

    #[test]
    fn test_volume_returns_none_when_missing() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum").expect("should parse");
        assert_eq!(fields.volume(), None);
    }

    #[test]
    fn test_volume_converts_from_percentage() {
        let fields =
            AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\t\t\t75").expect("should parse");
        assert_eq!(fields.volume(), Some(0.75));
    }

    #[test]
    fn test_duration_in_seconds_for_music() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\t\t\t\t240.5")
            .expect("should parse");
        assert_eq!(
            fields.duration_in_seconds(),
            Some(Duration::from_secs_f64(240.5))
        );
    }

    #[test]
    fn test_duration_in_millis_for_spotify() {
        let fields = AppleScriptFields::parse("Title\tArtist\tAlbum\t\t\t\t\t\t240500")
            .expect("should parse");
        assert_eq!(
            fields.duration_in_millis(),
            Some(Duration::from_millis(240_500))
        );
    }

    #[test]
    fn test_to_music_player_state_full() {
        let output =
            "Bohemian Rhapsody\tQueen\tA Night at the Opera\tQueen\t11\tplaying\t123.45\t75\t354.5";
        let fields = AppleScriptFields::parse(output).expect("should parse");
        let state = fields.to_music_player_state();

        assert_eq!(
            state,
            PlayerState {
                track: Track {
                    title: "Bohemian Rhapsody".to_string(),
                    artist: vec!["Queen".to_string()],
                    album: Some("A Night at the Opera".to_string()),
                    album_artist: Some(vec!["Queen".to_string()]),
                    track_number: Some(11),
                    duration: Some(Duration::from_secs_f64(354.5)),
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs_f64(123.45)),
                volume: Some(0.75),
            }
        );
    }

    #[test]
    fn test_to_spotify_player_state_full() {
        let output = "Bohemian Rhapsody\tQueen\tA Night at the Opera\tQueen\t11\tplaying\t123.45\t75\t354500";
        let fields = AppleScriptFields::parse(output).expect("should parse");
        let state = fields.to_spotify_player_state();

        assert_eq!(
            state,
            PlayerState {
                track: Track {
                    title: "Bohemian Rhapsody".to_string(),
                    artist: vec!["Queen".to_string()],
                    album: Some("A Night at the Opera".to_string()),
                    album_artist: Some(vec!["Queen".to_string()]),
                    track_number: Some(11),
                    duration: Some(Duration::from_millis(354_500)),
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs_f64(123.45)),
                volume: Some(0.75),
            }
        );
    }

    #[test]
    fn test_to_music_player_state_minimal() {
        let output = "Title\tArtist\tAlbum";
        let fields = AppleScriptFields::parse(output).expect("should parse");
        let state = fields.to_music_player_state();

        assert_eq!(
            state,
            PlayerState {
                track: Track {
                    title: "Title".to_string(),
                    artist: vec!["Artist".to_string()],
                    album: Some("Album".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: None,
                volume: None,
            }
        );
    }

    #[test]
    fn test_unicode_support() {
        let output =
            "春よ、来い\t松任谷由実\tThe Dancing Sun\t松任谷由実\t7\tplaying\t120\t65\t265";
        let fields = AppleScriptFields::parse(output).expect("should parse");

        assert_eq!(fields.title(), "春よ、来い");
        assert_eq!(fields.artist(), "松任谷由実");
        assert_eq!(fields.album(), Some("The Dancing Sun"));
    }

    #[test]
    fn test_special_characters() {
        let output = "Song & Title (Remix)\tArtist: Name\tAlbum - Edition\tVarious Artists\t3\tplaying\t30\t80\t200";
        let fields = AppleScriptFields::parse(output).expect("should parse");

        assert_eq!(fields.title(), "Song & Title (Remix)");
        assert_eq!(fields.artist(), "Artist: Name");
        assert_eq!(fields.album(), Some("Album - Edition"));
    }

    #[test]
    fn test_whitespace_preservation() {
        let output = "  Trimmed Title  \t  Trimmed Artist  \t  Trimmed Album  \t  Trimmed Artist  \t4\tplaying\t10\t90\t150";
        let fields = AppleScriptFields::parse(output).expect("should parse");

        // Whitespace is preserved
        assert_eq!(fields.title(), "  Trimmed Title  ");
        assert_eq!(fields.artist(), "  Trimmed Artist  ");
    }

    #[test]
    fn test_empty_album_returns_none() {
        let output = "Title\tArtist\t\tArtist\t1\tplaying\t0\t100\t180";
        let fields = AppleScriptFields::parse(output).expect("should parse");
        assert_eq!(fields.album(), None);
    }

    #[test]
    fn test_track_number_zero_filtered() {
        let output = "Title\tArtist\tAlbum\t\t0\tplaying\t30\t80\t200";
        let fields = AppleScriptFields::parse(output).expect("should parse");
        assert_eq!(fields.track_number(), None);
    }
    // PlayerMonitor tests

    #[test]
    fn test_player_monitor_new() {
        let monitor = PlayerMonitor::new();
        assert_eq!(monitor.players.len(), 0);
        assert_eq!(monitor.running.len(), 0);
    }

    #[test]
    fn test_player_monitor_first_player_added() {
        let mut monitor = PlayerMonitor::new();
        let track = create_test_track("Song 1");
        let state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        let events = monitor.process_player("Music", Some(state));

        // Should get: PlayerAdded, TrackChanged, StateChanged
        assert_eq!(
            events,
            vec![
                MediaEvent::PlayerAdded {
                    player_name: "Music".to_string()
                },
                MediaEvent::TrackChanged {
                    player_name: "Music".to_string(),
                    track,
                },
                MediaEvent::StateChanged {
                    player_name: "Music".to_string(),
                    state: PlaybackState::Playing
                },
            ]
        );
    }

    #[test]
    fn test_player_monitor_track_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track1 = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track1,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with different track (keep position similar to avoid position change event)
        let track2 = create_test_track("Song 2");
        let new_state = create_test_player_state_with_track(
            track2.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(11)), // Only 1 second difference
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should only detect track change
        assert_eq!(
            events,
            vec![MediaEvent::TrackChanged {
                player_name: "Music".to_string(),
                track: track2
            }]
        );
    }

    #[test]
    fn test_player_monitor_playback_state_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with different playback state
        let new_state = create_test_player_state_with_track(
            track,
            PlaybackState::Paused,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should detect state change
        assert_eq!(
            events,
            vec![MediaEvent::StateChanged {
                player_name: "Music".to_string(),
                state: PlaybackState::Paused
            }]
        );
    }

    #[test]
    fn test_player_monitor_position_seek() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with significant position jump
        let new_state = create_test_player_state_with_track(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(60)), // Jumped 50 seconds
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should detect position change
        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "Music".to_string(),
                position: Duration::from_secs(60)
            }]
        );
    }

    #[test]
    fn test_player_monitor_position_normal_playback() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with normal 1 second progression
        let new_state = create_test_player_state_with_track(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should not detect position change for normal playback
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_player_monitor_volume_changed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with different volume
        let new_state = create_test_player_state_with_track(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.5),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should detect volume change
        assert_eq!(
            events,
            vec![MediaEvent::VolumeChanged {
                player_name: "Music".to_string(),
                volume: 0.5
            }]
        );
    }

    #[test]
    fn test_player_monitor_player_removed() {
        let mut monitor = PlayerMonitor::new();

        // Initial state - player running
        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // Player stopped
        let events = monitor.process_player("Music", None);

        // Should detect player removal
        assert_eq!(
            events,
            vec![MediaEvent::PlayerRemoved {
                player_name: "Music".to_string()
            }]
        );

        // State should be cleared
        assert!(!monitor.players.contains_key("Music"));
    }

    #[test]
    fn test_player_monitor_multiple_players() {
        let mut monitor = PlayerMonitor::new();

        // Add Music.app
        let track1 = create_test_track("Song 1");
        let music_state = create_test_player_state_with_track(
            track1.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(music_state));
        assert_eq!(
            events,
            vec![
                MediaEvent::PlayerAdded {
                    player_name: "Music".to_string()
                },
                MediaEvent::TrackChanged {
                    player_name: "Music".to_string(),
                    track: track1
                },
                MediaEvent::StateChanged {
                    player_name: "Music".to_string(),
                    state: PlaybackState::Playing
                },
            ]
        );

        // Add Spotify
        let track2 = create_test_track("Song 2");
        let spotify_state = create_test_player_state_with_track(
            track2.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
            Some(0.7),
        );
        let events = monitor.process_player("Spotify", Some(spotify_state));
        assert_eq!(
            events,
            vec![
                MediaEvent::PlayerAdded {
                    player_name: "Spotify".to_string()
                },
                MediaEvent::TrackChanged {
                    player_name: "Spotify".to_string(),
                    track: track2
                },
                MediaEvent::StateChanged {
                    player_name: "Spotify".to_string(),
                    state: PlaybackState::Playing
                },
            ]
        );

        // Both should be tracked
        assert_eq!(monitor.players.len(), 2);
        assert!(monitor.players.contains_key("Music"));
        assert!(monitor.players.contains_key("Spotify"));
    }

    #[test]
    fn test_player_monitor_multiple_changes() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track1 = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track1,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // New state with multiple changes
        let track2 = create_test_track("Song 2");
        let new_state = create_test_player_state_with_track(
            track2.clone(),
            PlaybackState::Paused,
            Some(Duration::from_secs(60)),
            Some(0.5),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should detect all changes: track, state, position, volume
        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "Music".to_string(),
                    track: track2
                },
                MediaEvent::StateChanged {
                    player_name: "Music".to_string(),
                    state: PlaybackState::Paused
                },
                MediaEvent::PositionChanged {
                    player_name: "Music".to_string(),
                    position: Duration::from_secs(60)
                },
                MediaEvent::VolumeChanged {
                    player_name: "Music".to_string(),
                    volume: 0.5
                },
            ]
        );
    }

    #[test]
    fn test_player_monitor_no_changes() {
        let mut monitor = PlayerMonitor::new();

        // Initial state
        let track = create_test_track("Song 1");
        let state = create_test_player_state_with_track(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(state.clone()));

        // Same state again
        let events = monitor.process_player("Music", Some(state));

        // Should not generate any events
        assert_eq!(events, Vec::<MediaEvent>::new());
    }
}
