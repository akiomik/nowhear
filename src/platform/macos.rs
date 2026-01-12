//! macOS-specific implementation using AppleScript.

#[cfg(target_os = "macos")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use futures::stream::Stream;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Internal player state representation for macOS implementation.
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

/// Internal trait for abstracting player state retrieval.
///
/// This trait is used internally by the macOS implementation to allow
/// for dependency injection in tests. It is not part of the public API.
#[doc(hidden)]
pub trait PlayerStateProvider: Send + Sync {
    fn get_player_state(
        &self,
        player_name: &str,
    ) -> impl std::future::Future<Output = Result<Option<PlayerState>>> + Send;
    fn list_available_players(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>>> + Send;
}

/// AppleScript-based provider for macOS.
///
/// This provider uses AppleScript to query Music.app and Spotify for their
/// current playback state.
#[doc(hidden)]
pub struct AppleScriptProvider;

impl PlayerStateProvider for AppleScriptProvider {
    async fn get_player_state(&self, player_name: &str) -> Result<Option<PlayerState>> {
        match player_name {
            "Music" => Self::get_music_app_state().await,
            "Spotify" => Self::get_spotify_state().await,
            _ => Err(MediaWatcherError::PlayerNotFound(player_name.to_string())),
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
        let script = r#"
            if application "Music" is running then
                tell application "Music"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }

    async fn get_spotify_state() -> Result<Option<PlayerState>> {
        let script = r#"
            if application "Spotify" is running then
                tell application "Spotify"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }
}

/// macOS media watcher implementation using AppleScript.
///
/// # Implementation Note
///
/// This implementation uses AppleScript for querying player state rather than
/// `NSDistributedNotificationCenter`, which would require running on the main thread.
/// The AppleScript approach with periodic polling offers a simpler alternative that
/// works well in async contexts.
///
/// Note: This type is visible for technical reasons but should not be used directly.
/// Use `MediaWatcherBuilder` to create media watchers.
pub struct MacOSMediaWatcher<P: PlayerStateProvider = AppleScriptProvider> {
    provider: Arc<P>,
}

struct PlayerMonitor {
    players: std::collections::HashMap<String, PlayerState>,
    running: std::collections::HashMap<String, bool>,
}

impl PlayerMonitor {
    fn new() -> Self {
        Self {
            players: std::collections::HashMap::new(),
            running: std::collections::HashMap::new(),
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

impl MacOSMediaWatcher<AppleScriptProvider> {
    /// Creates a new macOS media watcher.
    ///
    /// Note: This is an internal API. Use `MediaWatcherBuilder` instead.
    #[doc(hidden)]
    pub fn new() -> Self {
        Self {
            provider: Arc::new(AppleScriptProvider),
        }
    }
}

impl<P: PlayerStateProvider + 'static> MacOSMediaWatcher<P> {
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

impl<P: PlayerStateProvider + 'static> MediaWatcher for MacOSMediaWatcher<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.provider.list_available_players().await
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
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
            MediaWatcherError::InternalError(format!("Failed to execute AppleScript: {e}"))
        })?;

    if !output.status.success() {
        return Err(MediaWatcherError::InternalError(format!(
            "AppleScript error: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_apple_script_output(output: &str) -> Option<PlayerState> {
    let parts: Vec<&str> = output.split('\t').collect();

    if parts.len() >= 3 {
        let track = Track {
            title: parts[0].to_string(),
            artist: vec![parts[1].to_string()],
            album: if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            },
            album_artist: None,
            track_number: None,
            duration: None,
            art_url: None,
        };

        // Parse the playback state from the 4th field if available
        let playback_state = if parts.len() >= 4 {
            #[allow(clippy::match_same_arms)]
            match parts[3].trim().to_lowercase().as_str() {
                "playing" | "fast forwarding" | "rewinding" => PlaybackState::Playing,
                "paused" => PlaybackState::Paused,
                "stopped" => PlaybackState::Stopped,
                _ => PlaybackState::Playing, // Default to Playing for unknown states
            }
        } else {
            PlaybackState::Playing // Default if state is not provided
        };

        // Parse position (in seconds) from the 5th field if available
        let position = if parts.len() >= 5 {
            parts[4]
                .trim()
                .parse::<f64>()
                .ok()
                .map(Duration::from_secs_f64)
        } else {
            None
        };

        // Parse volume from the 6th field if available
        let volume = if parts.len() >= 6 {
            parts[5].trim().parse::<f64>().ok().map(|vol| vol / 100.0) // Convert 0-100 to 0.0-1.0
        } else {
            None
        };

        Some(PlayerState {
            track,
            playback_state,
            position,
            volume,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

    // MacOSMediaWatcher tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_players() -> Result<()> {
        let provider = Arc::new(MockPlayerStateProvider::new());
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await?;
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
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await?;
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
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let mut players = watcher.list_players().await?;
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
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("Music").await?;
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
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("Music").await?;

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
        let watcher = MacOSMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("Spotify").await?;

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

    // AppleScript parsing tests

    #[test]
    fn test_parse_applescript_output_with_full_info() {
        let output = "Bohemian Rhapsody\tQueen\tA Night at the Opera\tplaying\t123.45\t75";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Bohemian Rhapsody".to_string(),
                    artist: vec!["Queen".to_string()],
                    album: Some("A Night at the Opera".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs_f64(123.45)),
                volume: Some(0.75),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_with_empty_album() {
        let output = "Test Song\tTest Artist\t\tplaying\t0\t100";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Test Song".to_string(),
                    artist: vec!["Test Artist".to_string()],
                    album: None,
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(0)),
                volume: Some(1.0),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_with_paused_state() {
        let output = "Some Track\tSome Artist\tSome Album\tpaused\t60.5\t50";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Some Track".to_string(),
                    artist: vec!["Some Artist".to_string()],
                    album: Some("Some Album".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs_f64(60.5)),
                volume: Some(0.5),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_with_special_characters() {
        let output = "Song & Title (Remix)\tArtist: Name\tAlbum - Edition\tplaying\t30\t80";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Song & Title (Remix)".to_string(),
                    artist: vec!["Artist: Name".to_string()],
                    album: Some("Album - Edition".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(30)),
                volume: Some(0.8),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_with_unicode() {
        let output = "春よ、来い\t松任谷由実\tThe Dancing Sun\tplaying\t120\t65";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "春よ、来い".to_string(),
                    artist: vec!["松任谷由実".to_string()],
                    album: Some("The Dancing Sun".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(120)),
                volume: Some(0.65),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_with_insufficient_parts() {
        let output = "Only Title\tOnly Artist";
        let result = parse_apple_script_output(output);

        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_applescript_output_with_empty_string() {
        let output = "";
        let result = parse_apple_script_output(output);

        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_applescript_output_with_single_field() {
        let output = "Just a title";
        let result = parse_apple_script_output(output);

        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_applescript_output_without_position_volume() {
        // Test when only 4 fields are provided (no position and volume)
        let output = "Title\tArtist\tAlbum\tplaying";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(
            player_state,
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
    fn test_parse_applescript_output_with_whitespace() {
        let output = "  Trimmed Title  \t  Trimmed Artist  \t  Trimmed Album  \tplaying\t10\t90";
        let player_state = parse_apple_script_output(output).expect("should success");

        // Note: The function doesn't trim individual fields, it preserves whitespace
        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "  Trimmed Title  ".to_string(),
                    artist: vec!["  Trimmed Artist  ".to_string()],
                    album: Some("  Trimmed Album  ".to_string()),
                    album_artist: None,
                    track_number: None,
                    duration: None,
                    art_url: None
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(10)),
                volume: Some(0.9),
            }
        );
    }

    #[test]
    fn test_parse_applescript_output_without_state() {
        // Test when only 3 fields are provided (no player state)
        let output = "Title\tArtist\tAlbum";
        let player_state = parse_apple_script_output(output).expect("should success");

        // Should default to Playing when state is not provided
        assert_eq!(
            player_state,
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
    fn test_parse_applescript_output_with_uppercase_state() {
        // Test case insensitivity
        let output = "Title\tArtist\tAlbum\tPLAYING\t45\t55";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn test_parse_applescript_output_with_stopped_state() {
        let output = "Title\tArtist\tAlbum\tstopped\t0\t0";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(player_state.playback_state, PlaybackState::Stopped);
    }

    #[test]
    fn test_parse_applescript_output_with_unknown_state() {
        // Test unknown state defaults to Playing
        let output = "Title\tArtist\tAlbum\tunknown\t0\t0";
        let player_state = parse_apple_script_output(output).expect("should success");

        assert_eq!(player_state.playback_state, PlaybackState::Playing);
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
