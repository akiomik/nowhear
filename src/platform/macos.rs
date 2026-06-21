//! macOS-specific implementation using AppleScript.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::error::{MediaSourceError, Result};
use crate::platform::state::{PlayerState, diff_player_state};
use crate::source::{EventStream, MediaSource};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};

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
        let script = include_str!("jxa/music.js");

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            let jxa_state: AppleScriptPlayerState = serde_json::from_str(&output)?;
            Ok(Some(jxa_state.into_music_player_state()))
        }
    }

    async fn get_spotify_state() -> Result<Option<PlayerState>> {
        let script = include_str!("jxa/spotify.js");

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            let jxa_state: AppleScriptPlayerState = serde_json::from_str(&output)?;
            Ok(Some(jxa_state.into_spotify_player_state()))
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
                    events.extend(diff_player_state(player_name, Some(last_state), &state));
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
            debug!("starting macOS media polling task");
            let mut monitor = PlayerMonitor::new();
            let mut interval = time::interval(Duration::from_secs(1));
            // Use Skip to avoid processing stale states when system is under load.
            // We only care about the current state, not catching up on missed polls.
            interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Poll both players in parallel
                let music_future = provider.get_player_state("Music");
                let spotify_future = provider.get_player_state("Spotify");
                let (music_state, spotify_state) = tokio::join!(music_future, spotify_future);

                // Process Music.app state
                #[cfg(feature = "tracing")]
                if let Err(ref e) = music_state {
                    tracing::debug!(player = "Music", error = %e, "failed to get player state, treating as not running");
                }
                let music_events = monitor.process_player("Music", music_state.ok().flatten());

                // Send Music.app events
                for event in music_events {
                    if tx.send(event).is_err() {
                        debug!("macOS media polling task shutting down: consumer dropped");
                        return;
                    }
                }

                // Process Spotify state
                #[cfg(feature = "tracing")]
                if let Err(ref e) = spotify_state {
                    tracing::debug!(player = "Spotify", error = %e, "failed to get player state, treating as not running");
                }
                let spotify_events =
                    monitor.process_player("Spotify", spotify_state.ok().flatten());

                // Send Spotify events
                for event in spotify_events {
                    if tx.send(event).is_err() {
                        debug!("macOS media polling task shutting down: consumer dropped");
                        return;
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
        .arg("-l")
        .arg("JavaScript")
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum AppleScriptPlaybackState {
    #[serde(rename = "stopped")]
    Stopped,
    #[serde(rename = "playing")]
    Playing,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "fast forwarding")]
    FastForwarding,
    #[serde(rename = "rewinding")]
    Rewinding,
}

/// Intermediate representation of player state from AppleScript (JXA).
///
/// This struct is used to deserialize JSON output from JXA scripts that query
/// Music.app or Spotify. It serves as a bridge between the AppleScript layer
/// and the internal [`PlayerState`] representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleScriptPlayerState {
    player_state: AppleScriptPlaybackState,
    player_position: f64,
    sound_volume: usize,
    track_name: String,
    track_artist: String,
    track_album: String,
    track_album_artist: String,
    track_album_artwork_url: Option<String>,
    track_number: u32,
    track_duration: f64,
}

impl AppleScriptPlayerState {
    pub const fn playback_state(&self) -> PlaybackState {
        match self.player_state {
            AppleScriptPlaybackState::Paused => PlaybackState::Paused,
            AppleScriptPlaybackState::Stopped => PlaybackState::Stopped,
            _ => PlaybackState::Playing,
        }
    }

    pub fn into_music_track(self) -> Track {
        Track {
            title: self.track_name,
            artist: if self.track_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_artist]
            },
            album: if self.track_album.is_empty() {
                None
            } else {
                Some(self.track_album)
            },
            album_artist: if self.track_album_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_album_artist]
            },
            track_number: Some(self.track_number),
            duration: Some(Duration::from_secs_f64(self.track_duration)),
            art_url: self.track_album_artwork_url.filter(|s| !s.is_empty()),
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn into_spotify_track(self) -> Track {
        Track {
            title: self.track_name,
            artist: if self.track_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_artist]
            },
            album: if self.track_album.is_empty() {
                None
            } else {
                Some(self.track_album)
            },
            album_artist: if self.track_album_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_album_artist]
            },
            track_number: Some(self.track_number),
            duration: Some(Duration::from_millis(self.track_duration as u64)),
            art_url: self.track_album_artwork_url.filter(|s| !s.is_empty()),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn into_music_player_state(self) -> PlayerState {
        let player_position = Duration::from_secs_f64(self.player_position);
        let sound_volume = self.sound_volume as f64 / 100.0;
        let playback_state = self.playback_state();

        PlayerState {
            track: self.into_music_track(),
            playback_state,
            position: Some(player_position),
            volume: Some(sound_volume),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn into_spotify_player_state(self) -> PlayerState {
        let player_position = Duration::from_secs_f64(self.player_position);
        let sound_volume = self.sound_volume as f64 / 100.0;
        let playback_state = self.playback_state();

        PlayerState {
            track: self.into_spotify_track(),
            playback_state,
            position: Some(player_position),
            volume: Some(sound_volume),
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
            album_artist: vec![],
            track_number: None,
            duration: Some(Duration::from_mins(3)),
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

        // New state with a different track. The new track's position is reported as a fresh
        // PositionChanged: a track change resets the seek baseline, so the new track's position is
        // emitted regardless of how close it is to the previous track's position.
        let track2 = create_test_track("Song 2");
        let new_state = create_test_player_state_with_track(
            track2.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(11)),
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "Music".to_string(),
                    track: track2
                },
                MediaEvent::PositionChanged {
                    player_name: "Music".to_string(),
                    position: Duration::from_secs(11)
                },
            ]
        );
    }

    #[test]
    fn test_player_monitor_metadata_only_change_not_a_track_change() {
        let mut monitor = PlayerMonitor::new();

        let track = create_test_track("Song 1");
        let initial_state = create_test_player_state_with_track(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor.process_player("Music", Some(initial_state));

        // Same title and artist, but late-loading metadata (album) changes. Track identity is
        // title + artist, so this is NOT reported as a track change.
        let mut updated_track = track;
        updated_track.album = Some("Different Album".to_string());
        let new_state = create_test_player_state_with_track(
            updated_track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        assert_eq!(events, Vec::<MediaEvent>::new());
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
            Some(Duration::from_mins(1)), // Jumped 50 seconds
            Some(0.8),
        );
        let events = monitor.process_player("Music", Some(new_state));

        // Should detect position change
        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "Music".to_string(),
                position: Duration::from_mins(1)
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
            Some(Duration::from_mins(1)),
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
                    position: Duration::from_mins(1)
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

    // AppleScriptPlayerState tests

    #[test]
    fn test_applescript_player_state_deserialize_playing() -> Result<()> {
        let json = r#"{
            "playerState": "playing",
            "playerPosition": 45.5,
            "soundVolume": 80,
            "trackName": "Test Song",
            "trackArtist": "Test Artist",
            "trackAlbum": "Test Album",
            "trackAlbumArtist": "Test Album Artist",
            "trackNumber": 3,
            "trackDuration": 180.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(
            state,
            AppleScriptPlayerState {
                player_state: AppleScriptPlaybackState::Playing,
                player_position: 45.5,
                sound_volume: 80,
                track_name: "Test Song".to_string(),
                track_artist: "Test Artist".to_string(),
                track_album: "Test Album".to_string(),
                track_album_artist: "Test Album Artist".to_string(),
                track_album_artwork_url: None,
                track_number: 3,
                track_duration: 180.0,
            }
        );

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_paused() -> Result<()> {
        let json = r#"{
            "playerState": "paused",
            "playerPosition": 30.0,
            "soundVolume": 50,
            "trackName": "Paused Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 1,
            "trackDuration": 200.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Paused);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_stopped() -> Result<()> {
        let json = r#"{
            "playerState": "stopped",
            "playerPosition": 0.0,
            "soundVolume": 70,
            "trackName": "Stopped Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 2,
            "trackDuration": 150.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Stopped);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_fast_forwarding() -> Result<()> {
        let json = r#"{
            "playerState": "fast forwarding",
            "playerPosition": 60.0,
            "soundVolume": 90,
            "trackName": "Fast Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 5,
            "trackDuration": 240.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::FastForwarding);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_rewinding() -> Result<()> {
        let json = r#"{
            "playerState": "rewinding",
            "playerPosition": 20.0,
            "soundVolume": 60,
            "trackName": "Rewind Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 4,
            "trackDuration": 210.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Rewinding);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_track_album_artwork_url() -> Result<()> {
        let json = r#"{
            "playerState": "playing",
            "playerPosition": 45.5,
            "soundVolume": 80,
            "trackName": "Test Song",
            "trackArtist": "Test Artist",
            "trackAlbum": "Test Album",
            "trackAlbumArtist": "Test Album Artist",
            "trackAlbumArtworkUrl": "https://example.com/image/deadbeef",
            "trackNumber": 3,
            "trackDuration": 180.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(
            state,
            AppleScriptPlayerState {
                player_state: AppleScriptPlaybackState::Playing,
                player_position: 45.5,
                sound_volume: 80,
                track_name: "Test Song".to_string(),
                track_artist: "Test Artist".to_string(),
                track_album: "Test Album".to_string(),
                track_album_artist: "Test Album Artist".to_string(),
                track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
                track_number: 3,
                track_duration: 180.0,
            }
        );

        Ok(())
    }

    #[test]
    fn test_applescript_playback_state_conversion() {
        let test_cases = vec![
            (AppleScriptPlaybackState::Playing, PlaybackState::Playing),
            (AppleScriptPlaybackState::Paused, PlaybackState::Paused),
            (AppleScriptPlaybackState::Stopped, PlaybackState::Stopped),
            (
                AppleScriptPlaybackState::FastForwarding,
                PlaybackState::Playing,
            ),
            (AppleScriptPlaybackState::Rewinding, PlaybackState::Playing),
        ];

        for (applescript_state, expected_state) in test_cases {
            let state = AppleScriptPlayerState {
                player_state: applescript_state,
                player_position: 0.0,
                sound_volume: 50,
                track_name: "Test".to_string(),
                track_artist: "Artist".to_string(),
                track_album: "Album".to_string(),
                track_album_artist: "Album Artist".to_string(),
                track_album_artwork_url: None,
                track_number: 1,
                track_duration: 100.0,
            };

            assert_eq!(state.playback_state(), expected_state);
        }
    }

    #[test]
    fn test_applescript_player_state_into_music_track() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 45.5,
            sound_volume: 80,
            track_name: "Bohemian Rhapsody".to_string(),
            track_artist: "Queen".to_string(),
            track_album: "A Night at the Opera".to_string(),
            track_album_artist: "Queen".to_string(),
            track_album_artwork_url: None,
            track_number: 11,
            track_duration: 354.0, // seconds
        };

        let track = state.into_music_track();

        assert_eq!(
            track,
            Track {
                title: "Bohemian Rhapsody".to_string(),
                artist: vec!["Queen".to_string()],
                album: Some("A Night at the Opera".to_string()),
                album_artist: vec!["Queen".to_string()],
                track_number: Some(11),
                duration: Some(Duration::from_secs_f64(354.0)),
                art_url: None,
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_spotify_track() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 30.0,
            sound_volume: 70,
            track_name: "Stairway to Heaven".to_string(),
            track_artist: "Led Zeppelin".to_string(),
            track_album: "Led Zeppelin IV".to_string(),
            track_album_artist: "Led Zeppelin".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 4,
            track_duration: 482_000.0, // milliseconds
        };

        let track = state.into_spotify_track();

        assert_eq!(
            track,
            Track {
                title: "Stairway to Heaven".to_string(),
                artist: vec!["Led Zeppelin".to_string()],
                album: Some("Led Zeppelin IV".to_string()),
                album_artist: vec!["Led Zeppelin".to_string()],
                track_number: Some(4),
                duration: Some(Duration::from_secs(482)),
                art_url: Some("https://example.com/image/deadbeef".to_owned()),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_music_player_state() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Paused,
            player_position: 120.5,
            sound_volume: 75,
            track_name: "Test Track".to_string(),
            track_artist: "Test Artist".to_string(),
            track_album: "Test Album".to_string(),
            track_album_artist: "Test Album Artist".to_string(),
            track_album_artwork_url: None,
            track_number: 2,
            track_duration: 240.0,
        };

        let player_state = state.into_music_player_state();

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Test Track".to_string(),
                    artist: vec!["Test Artist".to_string()],
                    album: Some("Test Album".to_string()),
                    album_artist: vec!["Test Album Artist".to_string()],
                    track_number: Some(2),
                    duration: Some(Duration::from_secs_f64(240.0)),
                    art_url: None,
                },
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs_f64(120.5)),
                volume: Some(0.75),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_spotify_player_state() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 65.25,
            sound_volume: 85,
            track_name: "Spotify Track".to_string(),
            track_artist: "Spotify Artist".to_string(),
            track_album: "Spotify Album".to_string(),
            track_album_artist: "Spotify Album Artist".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 7,
            track_duration: 195_000.0, // milliseconds for Spotify
        };

        let player_state = state.into_spotify_player_state();

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Spotify Track".to_string(),
                    artist: vec!["Spotify Artist".to_string()],
                    album: Some("Spotify Album".to_string()),
                    album_artist: vec!["Spotify Album Artist".to_string()],
                    track_number: Some(7),
                    duration: Some(Duration::from_secs(195)),
                    art_url: Some("https://example.com/image/deadbeef".to_owned()),
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs_f64(65.25)),
                volume: Some(0.85),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_duration_difference() {
        // Music.app uses seconds for duration
        let music_state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: "Artist".to_string(),
            track_album: "Album".to_string(),
            track_album_artist: "Album Artist".to_string(),
            track_album_artwork_url: None,
            track_number: 1,
            track_duration: 180.0, // 180 seconds
        };

        // Spotify uses milliseconds for duration
        let spotify_state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: "Artist".to_string(),
            track_album: "Album".to_string(),
            track_album_artist: "Album Artist".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 1,
            track_duration: 180_000.0, // 180000 milliseconds = 180 seconds
        };

        let music_track = music_state.into_music_track();
        let spotify_track = spotify_state.into_spotify_track();

        // Both should represent the same duration
        assert_eq!(music_track.duration, Some(Duration::from_mins(3)));
        assert_eq!(spotify_track.duration, Some(Duration::from_mins(3)));
        assert_eq!(music_track.duration, spotify_track.duration);
    }
}
