#[cfg(target_os = "linux")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use mpris::{Metadata, PlaybackStatus as MprisPlaybackStatus, Player, PlayerFinder};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{interval, sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;

pub struct LinuxMediaWatcher {
    finder: PlayerFinder,
    // Cache of known players
    players: Arc<RwLock<HashMap<String, Player>>>,
}

/// Monitors player state changes and generates events
struct PlayerMonitor {
    known_players: HashMap<String, Player>,
    last_states: HashMap<String, PlayerState>,
}

/// Represents the last known state of a player
#[derive(Clone, Debug)]
struct PlayerState {
    track: Option<Track>,
    playback_state: PlaybackState,
    position: Option<Duration>,
    volume: Option<f64>,
}

impl PlayerMonitor {
    fn new() -> Self {
        Self {
            known_players: HashMap::new(),
            last_states: HashMap::new(),
        }
    }

    fn known_players(&self) -> &HashMap<String, Player> {
        &self.known_players
    }

    /// Process current players and generate events for any changes
    fn process_players(&mut self, current_players: Vec<Player>) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        let mut current_names = HashMap::new();

        // Process each player and detect changes
        for player in current_players {
            let player_name = extract_player_name(player.bus_name());
            current_names.insert(player_name.clone(), player.clone());

            // Detect new players
            if !self.known_players.contains_key(&player_name) {
                events.push(MediaEvent::PlayerAdded {
                    player_name: player_name.clone(),
                });
            }

            // Get current state and detect changes
            if let Some(current_state) = Self::get_player_state(&player) {
                self.detect_state_changes(&player_name, &current_state, &mut events);
                self.last_states.insert(player_name, current_state);
            }
        }

        // Detect removed players
        for player_name in self.known_players.keys() {
            if !current_names.contains_key(player_name) {
                events.push(MediaEvent::PlayerRemoved {
                    player_name: player_name.clone(),
                });
                self.last_states.remove(player_name);
            }
        }

        self.known_players = current_names;
        events
    }

    /// Get the current state of a player
    fn get_player_state(player: &Player) -> Option<PlayerState> {
        let metadata = player.get_metadata().ok()?;
        let playback_status = player.get_playback_status().ok()?;

        let track = if metadata.title().is_some() || !metadata.artists().unwrap_or(&[]).is_empty() {
            Some(parse_metadata(&metadata))
        } else {
            None
        };

        let position = player
            .get_position()
            .ok()
            .map(|micros| Duration::from_micros(micros as u64));

        let volume = player.get_volume().ok();

        Some(PlayerState {
            track,
            playback_state: parse_playback_status(playback_status),
            position,
            volume,
        })
    }

    /// Detect state changes and generate appropriate events
    fn detect_state_changes(
        &self,
        player_name: &str,
        current: &PlayerState,
        events: &mut Vec<MediaEvent>,
    ) {
        match self.last_states.get(player_name) {
            Some(last) => {
                // Check for track changes
                if current.track != last.track && current.track.is_some() {
                    events.push(MediaEvent::TrackChanged {
                        player_name: player_name.to_string(),
                        track: current.track.clone().unwrap(),
                    });
                }

                // Check for playback state changes
                if current.playback_state != last.playback_state {
                    events.push(MediaEvent::StateChanged {
                        player_name: player_name.to_string(),
                        state: current.playback_state,
                    });
                }

                // Check for position changes (seeks)
                Self::detect_position_change(player_name, current, last, events);

                // Check for volume changes
                if current.volume != last.volume && current.volume.is_some() {
                    events.push(MediaEvent::VolumeChanged {
                        player_name: player_name.to_string(),
                        volume: current.volume.unwrap(),
                    });
                }
            }
            None => {
                // First time seeing this player with valid state
                if let Some(track) = &current.track {
                    events.push(MediaEvent::TrackChanged {
                        player_name: player_name.to_string(),
                        track: track.clone(),
                    });
                    events.push(MediaEvent::StateChanged {
                        player_name: player_name.to_string(),
                        state: current.playback_state,
                    });
                }
            }
        }
    }

    /// Detect significant position changes (seeks)
    fn detect_position_change(
        player_name: &str,
        current: &PlayerState,
        last: &PlayerState,
        events: &mut Vec<MediaEvent>,
    ) {
        if let (Some(pos), Some(last_pos)) = (current.position, last.position) {
            let diff = if pos > last_pos {
                pos - last_pos
            } else {
                last_pos - pos
            };

            // If difference is more than 2 seconds, it's probably a seek
            if diff > Duration::from_secs(2) {
                events.push(MediaEvent::PositionChanged {
                    player_name: player_name.to_string(),
                    position: pos,
                });
            }
        }
    }
}

impl LinuxMediaWatcher {
    pub async fn new() -> Result<Self> {
        let finder =
            PlayerFinder::new().map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        Ok(Self {
            finder,
            players: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    async fn discover_players(&self) -> Result<Vec<String>> {
        // MPRIS players have names like "org.mpris.MediaPlayer2.spotify"
        // Use the mpris crate to find all MPRIS players
        let players = self
            .finder
            .find_all()
            .map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        let player_names: Vec<String> = players
            .iter()
            .map(|p| extract_player_name(p.bus_name()))
            .collect();

        // Update cache
        let mut cache = self.players.write().await;
        cache.clear();
        for player in players {
            let name = extract_player_name(player.bus_name());
            cache.insert(name, player);
        }

        Ok(player_names)
    }

    async fn get_player_info(&self, player_name: &str) -> Result<PlayerInfo> {
        // First, try to get from cache
        let players = self.players.read().await;
        let player = players
            .get(player_name)
            .ok_or_else(|| MediaWatcherError::PlayerNotFound(player_name.to_string()))?;

        // Query player metadata and status
        let metadata = player
            .get_metadata()
            .map_err(|e| MediaWatcherError::ParseError(e.to_string()))?;

        let playback_status = player
            .get_playback_status()
            .map_err(|e| MediaWatcherError::ParseError(e.to_string()))?;

        let position = player
            .get_position()
            .ok()
            .map(|micros| Duration::from_micros(micros as u64));

        let volume = player.get_volume().ok();

        let current_track =
            if metadata.title().is_some() || !metadata.artists().unwrap_or(&[]).is_empty() {
                Some(parse_metadata(&metadata))
            } else {
                None
            };

        Ok(PlayerInfo {
            player_name: player_name.to_string(),
            current_track,
            playback_state: parse_playback_status(playback_status),
            position,
            volume,
        })
    }

    fn create_event_stream_impl(&self) -> impl Stream<Item = MediaEvent> {
        let finder = self.finder.clone();
        let players_cache = self.players.clone();

        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut monitor = PlayerMonitor::new();
            let mut poll_interval = interval(Duration::from_millis(500));

            loop {
                poll_interval.tick().await;

                // Discover current players
                let current_players = match finder.find_all() {
                    Ok(players) => players,
                    Err(_) => {
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                // Process all player changes and generate events
                let events = monitor.process_players(current_players);

                // Send all events
                for event in events {
                    if tx.send(event).is_err() {
                        return; // Receiver dropped
                    }
                }

                // Update cache
                if let Ok(mut cache) = players_cache.try_write() {
                    *cache = monitor.known_players().clone();
                }
            }
        });

        UnboundedReceiverStream::new(rx)
    }
}

#[async_trait]
impl MediaWatcher for LinuxMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.discover_players().await
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        self.get_player_info(player_name).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.create_event_stream_impl();
        Ok(Box::pin(stream))
    }
}

// Helper functions to convert MPRIS data to our types

/// Extract player name from D-Bus bus name
/// e.g., "org.mpris.MediaPlayer2.spotify" -> "spotify"
fn extract_player_name(bus_name: &str) -> String {
    bus_name
        .strip_prefix("org.mpris.MediaPlayer2.")
        .unwrap_or(bus_name)
        .to_string()
}

fn parse_metadata(metadata: &Metadata) -> Track {
    let title = metadata
        .title()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    let artist = metadata
        .artists()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.to_string())
        .collect();

    let album = metadata.album_name().map(|s| s.to_string());

    let album_artist = metadata
        .album_artists()
        .map(|artists| artists.iter().map(|s| s.to_string()).collect());

    let track_number = metadata.track_number();

    let duration = metadata
        .length()
        .map(|micros| Duration::from_micros(micros as u64));

    let art_url = metadata.art_url().map(|s| s.to_string());

    Track {
        title,
        artist,
        album,
        album_artist,
        track_number,
        duration,
        art_url,
    }
}

fn parse_playback_status(status: MprisPlaybackStatus) -> PlaybackState {
    match status {
        MprisPlaybackStatus::Playing => PlaybackState::Playing,
        MprisPlaybackStatus::Paused => PlaybackState::Paused,
        MprisPlaybackStatus::Stopped => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpris::metadata::Value;
    use std::collections::HashMap;

    // Helper function to create Metadata for testing
    fn create_metadata(values: HashMap<String, Value>) -> Metadata {
        Metadata::from(values)
    }

    #[test]
    fn test_extract_player_name_with_prefix() {
        assert_eq!(
            extract_player_name("org.mpris.MediaPlayer2.spotify"),
            "spotify"
        );
        assert_eq!(extract_player_name("org.mpris.MediaPlayer2.vlc"), "vlc");
        assert_eq!(
            extract_player_name("org.mpris.MediaPlayer2.rhythmbox"),
            "rhythmbox"
        );
    }

    #[test]
    fn test_extract_player_name_without_prefix() {
        assert_eq!(extract_player_name("custom.player"), "custom.player");
        assert_eq!(extract_player_name("player"), "player");
    }

    #[test]
    fn test_extract_player_name_with_instance() {
        // Some players add instance numbers
        assert_eq!(
            extract_player_name("org.mpris.MediaPlayer2.chromium.instance1234"),
            "chromium.instance1234"
        );
    }

    #[test]
    fn test_parse_metadata_with_full_info() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("Test Song".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![Value::String("Test Artist".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            Value::String("Test Album".to_string()),
        );
        values.insert(
            "xesam:albumArtist".to_string(),
            Value::Array(vec![Value::String("Album Artist".to_string())]),
        );
        values.insert("xesam:trackNumber".to_string(), Value::I32(5));
        values.insert("mpris:length".to_string(), Value::I64(180_000_000)); // microseconds
        values.insert(
            "mpris:artUrl".to_string(),
            Value::String("file:///path/to/art.jpg".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Test Song");
        assert_eq!(track.artist, vec!["Test Artist"]);
        assert_eq!(track.album, Some("Test Album".to_string()));
        assert_eq!(track.album_artist, Some(vec!["Album Artist".to_string()]));
        assert_eq!(track.track_number, Some(5));
        assert_eq!(track.duration, Some(Duration::from_secs(180)));
        assert_eq!(track.art_url, Some("file:///path/to/art.jpg".to_string()));
    }

    #[test]
    fn test_parse_metadata_with_multiple_artists() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("Collaboration Song".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![
                Value::String("Artist 1".to_string()),
                Value::String("Artist 2".to_string()),
                Value::String("Artist 3".to_string()),
            ]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Collaboration Song");
        assert_eq!(track.artist, vec!["Artist 1", "Artist 2", "Artist 3"]);
    }

    #[test]
    fn test_parse_metadata_with_minimal_info() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("Minimal Song".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Minimal Song");
        assert_eq!(track.artist, Vec::<String>::new());
        assert_eq!(track.album, None);
        assert_eq!(track.album_artist, None);
        assert_eq!(track.track_number, None);
        assert_eq!(track.duration, None);
        assert_eq!(track.art_url, None);
    }

    #[test]
    fn test_parse_metadata_without_title() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![Value::String("Artist Only".to_string())]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Unknown");
        assert_eq!(track.artist, vec!["Artist Only"]);
    }

    #[test]
    fn test_parse_metadata_with_empty_metadata() {
        let values = HashMap::new();
        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Unknown");
        assert_eq!(track.artist, Vec::<String>::new());
        assert_eq!(track.album, None);
    }

    #[test]
    fn test_parse_metadata_with_unicode() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("テスト曲".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![Value::String("アーティスト名".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            Value::String("アルバム🎵".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "テスト曲");
        assert_eq!(track.artist, vec!["アーティスト名"]);
        assert_eq!(track.album, Some("アルバム🎵".to_string()));
    }

    #[test]
    fn test_parse_metadata_with_special_characters() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("Song: The \"Best\" & Greatest (2024)".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![Value::String("Artist's Name / Band".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            Value::String("Album <Special Edition>".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(track.title, "Song: The \"Best\" & Greatest (2024)");
        assert_eq!(track.artist, vec!["Artist's Name / Band"]);
        assert_eq!(track.album, Some("Album <Special Edition>".to_string()));
    }

    #[test]
    fn test_parse_metadata_with_multiple_album_artists() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            Value::String("Compilation Track".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            Value::Array(vec![Value::String("Track Artist".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            Value::String("Various Artists".to_string()),
        );
        values.insert(
            "xesam:albumArtist".to_string(),
            Value::Array(vec![
                Value::String("Artist A".to_string()),
                Value::String("Artist B".to_string()),
            ]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track.album_artist,
            Some(vec!["Artist A".to_string(), "Artist B".to_string()])
        );
    }

    #[test]
    fn test_parse_playback_status_playing() {
        assert_eq!(
            parse_playback_status(MprisPlaybackStatus::Playing),
            PlaybackState::Playing
        );
    }

    #[test]
    fn test_parse_playback_status_paused() {
        assert_eq!(
            parse_playback_status(MprisPlaybackStatus::Paused),
            PlaybackState::Paused
        );
    }

    #[test]
    fn test_parse_playback_status_stopped() {
        assert_eq!(
            parse_playback_status(MprisPlaybackStatus::Stopped),
            PlaybackState::Stopped
        );
    }

    // PlayerMonitor tests

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

    fn create_test_state(
        track: Option<Track>,
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

    #[test]
    fn test_player_monitor_new() {
        let monitor = PlayerMonitor::new();
        assert_eq!(monitor.known_players().len(), 0);
        assert_eq!(monitor.last_states.len(), 0);
    }

    #[test]
    fn test_detect_state_changes_first_time_with_track() {
        let monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        let state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        monitor.detect_state_changes("spotify", &state, &mut events);

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], MediaEvent::TrackChanged { .. }));
        assert!(matches!(events[1], MediaEvent::StateChanged { .. }));
    }

    #[test]
    fn test_detect_state_changes_first_time_without_track() {
        let monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        let state = create_test_state(None, PlaybackState::Stopped, None, None);

        monitor.detect_state_changes("spotify", &state, &mut events);

        // No events should be generated for a player with no track
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_detect_state_changes_track_changed() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let initial_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with different track
        let new_state = create_test_state(
            Some(create_test_track("Song 2")),
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
            Some(0.8),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        assert_eq!(events.len(), 1);
        if let MediaEvent::TrackChanged { player_name, track } = &events[0] {
            assert_eq!(player_name, "spotify");
            assert_eq!(track.title, "Song 2");
        } else {
            panic!("Expected TrackChanged event");
        }
    }

    #[test]
    fn test_detect_state_changes_playback_state_changed() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let initial_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with different playback state
        let new_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Paused,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        assert_eq!(events.len(), 1);
        if let MediaEvent::StateChanged { player_name, state } = &events[0] {
            assert_eq!(player_name, "spotify");
            assert_eq!(*state, PlaybackState::Paused);
        } else {
            panic!("Expected StateChanged event");
        }
    }

    #[test]
    fn test_detect_state_changes_volume_changed() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let initial_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with different volume
        let new_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.5),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        assert_eq!(events.len(), 1);
        if let MediaEvent::VolumeChanged {
            player_name,
            volume,
        } = &events[0]
        {
            assert_eq!(player_name, "spotify");
            assert_eq!(*volume, 0.5);
        } else {
            panic!("Expected VolumeChanged event");
        }
    }

    #[test]
    fn test_detect_state_changes_no_changes() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), state.clone());

        // Same state again
        monitor.detect_state_changes("spotify", &state, &mut events);

        // No events should be generated
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_detect_position_change_significant_forward_seek() {
        let mut events = Vec::new();

        let last_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        let current_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(60)), // Jumped forward by 50 seconds
            Some(0.8),
        );

        PlayerMonitor::detect_position_change("spotify", &current_state, &last_state, &mut events);

        assert_eq!(events.len(), 1);
        if let MediaEvent::PositionChanged {
            player_name,
            position,
        } = &events[0]
        {
            assert_eq!(player_name, "spotify");
            assert_eq!(*position, Duration::from_secs(60));
        } else {
            panic!("Expected PositionChanged event");
        }
    }

    #[test]
    fn test_detect_position_change_significant_backward_seek() {
        let mut events = Vec::new();

        let last_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(60)),
            Some(0.8),
        );

        let current_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)), // Jumped backward by 50 seconds
            Some(0.8),
        );

        PlayerMonitor::detect_position_change("spotify", &current_state, &last_state, &mut events);

        assert_eq!(events.len(), 1);
        if let MediaEvent::PositionChanged {
            player_name,
            position,
        } = &events[0]
        {
            assert_eq!(player_name, "spotify");
            assert_eq!(*position, Duration::from_secs(10));
        } else {
            panic!("Expected PositionChanged event");
        }
    }

    #[test]
    fn test_detect_position_change_normal_playback() {
        let mut events = Vec::new();

        let last_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        let current_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(11)), // Normal 1 second progression
            Some(0.8),
        );

        PlayerMonitor::detect_position_change("spotify", &current_state, &last_state, &mut events);

        // No event for normal playback progression
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_detect_position_change_exactly_2_seconds() {
        let mut events = Vec::new();

        let last_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        let current_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(12)), // Exactly 2 seconds
            Some(0.8),
        );

        PlayerMonitor::detect_position_change("spotify", &current_state, &last_state, &mut events);

        // Exactly 2 seconds should NOT trigger (threshold is > 2 seconds)
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_detect_position_change_no_position_info() {
        let mut events = Vec::new();

        let last_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            None, // No position info
            Some(0.8),
        );

        let current_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            None, // No position info
            Some(0.8),
        );

        PlayerMonitor::detect_position_change("spotify", &current_state, &last_state, &mut events);

        // No event when position info is missing
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_detect_state_changes_multiple_changes() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let initial_state = create_test_state(
            Some(create_test_track("Song 1")),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with multiple changes
        let new_state = create_test_state(
            Some(create_test_track("Song 2")),
            PlaybackState::Paused,
            Some(Duration::from_secs(70)), // Significant position change
            Some(0.5),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        // Should detect: track change, state change, position change, volume change
        assert_eq!(events.len(), 4);

        let has_track_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::TrackChanged { .. }));
        let has_state_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::StateChanged { .. }));
        let has_position_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::PositionChanged { .. }));
        let has_volume_change = events
            .iter()
            .any(|e| matches!(e, MediaEvent::VolumeChanged { .. }));

        assert!(has_track_change);
        assert!(has_state_change);
        assert!(has_position_change);
        assert!(has_volume_change);
    }
}
