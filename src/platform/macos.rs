//! macOS-specific implementation using in-process OSA (JavaScript-for-Automation).

mod osa;
mod provider;
mod running;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::error::Result;
use crate::platform::state::{PlayerState, diff_player_state};
use crate::source::{EventStream, MediaSource};
use crate::types::{MediaEvent, PlayerInfo};

pub use provider::{AppleScriptProvider, PlayerStateProvider};

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
/// This implementation queries player state via the Open Scripting Architecture
/// (OSA) rather than `NSDistributedNotificationCenter`, which would require
/// running on the main thread. Periodic polling (every 1 second) is a simpler
/// alternative that works well in async contexts and, unlike notifications, can
/// observe volume and position changes.
///
/// The polling interval is fixed at 1 second, which provides a good balance
/// between responsiveness and system resource usage. Each poll runs a single
/// JavaScript-for-Automation script that returns both Music.app and Spotify in
/// one call. The script is executed in-process on a dedicated worker thread (see
/// `osa`) instead of spawning an `osascript` subprocess per
/// poll, which eliminates the per-poll `posix_spawn` cost.
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`crate::source::MediaSourceBuilder`] to create media sources, which will
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

                // Poll both players with a single in-process OSA script execution.
                let states = provider.get_all_player_states().await;

                #[cfg(feature = "tracing")]
                if let Err(ref e) = states {
                    tracing::debug!(error = %e, "failed to get player states, treating all as not running");
                }

                // A failed query is treated as "no players running".
                let (music_state, spotify_state) = match states {
                    Ok(states) => (states.music, states.spotify),
                    Err(_) => (None, None),
                };

                // Process Music.app state
                let music_events = monitor.process_player("Music", music_state);

                // Send Music.app events
                for event in music_events {
                    if tx.send(event).is_err() {
                        debug!("macOS media polling task shutting down: consumer dropped");
                        return;
                    }
                }

                // Process Spotify state
                let spotify_events = monitor.process_player("Spotify", spotify_state);

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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::provider::AllPlayerStates;
    use super::*;
    use crate::types::{PlaybackState, Track};

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

        async fn get_all_player_states(&self) -> Result<AllPlayerStates> {
            Ok(AllPlayerStates {
                music: self.states.get("Music").cloned().flatten(),
                spotify: self.states.get("Spotify").cloned().flatten(),
            })
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

    #[tokio::test]
    async fn test_get_all_player_states_returns_both() -> Result<()> {
        let music_state = create_test_player_state_with_track(
            create_test_track("Music Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let spotify_state = create_test_player_state_with_track(
            create_test_track("Spotify Song"),
            PlaybackState::Paused,
            Some(Duration::from_secs(5)),
            Some(0.7),
        );
        let provider = MockPlayerStateProvider::new()
            .with_player("Music", Some(music_state.clone()))
            .with_player("Spotify", Some(spotify_state.clone()));

        let states = provider.get_all_player_states().await?;
        assert_eq!(states.music, Some(music_state));
        assert_eq!(states.spotify, Some(spotify_state));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_all_player_states_with_missing_players() -> Result<()> {
        let music_state = create_test_player_state_with_track(
            create_test_track("Music Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        let provider = MockPlayerStateProvider::new()
            .with_player("Music", Some(music_state.clone()))
            .with_player("Spotify", None);

        let states = provider.get_all_player_states().await?;
        assert_eq!(states.music, Some(music_state));
        assert_eq!(states.spotify, None);

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

    // MacOSMediaSource::new() — wraps AppleScriptProvider in an Arc, no I/O

    #[test]
    fn test_macos_media_source_new() {
        let _source = MacOSMediaSource::new();
    }

    // event_stream() with mock provider — exercises the public API without OSA

    #[tokio::test]
    async fn test_event_stream_returns_ok_with_mock() -> Result<()> {
        let provider = Arc::new(MockPlayerStateProvider::new());
        let source = MacOSMediaSource::with_provider(provider);

        let result = source.event_stream().await;
        assert!(result.is_ok());

        Ok(())
    }
}
