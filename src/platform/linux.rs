#[cfg(target_os = "linux")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use futures::stream::Stream;
use mpris::{Metadata, PlaybackStatus as MprisPlaybackStatus, Player, PlayerFinder};
use std::collections::HashMap;
use std::string::ToString;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Internal trait for abstracting player discovery mechanisms.
///
/// This trait is used internally by the Linux implementation to allow
/// for dependency injection in tests. It is not part of the public API.
#[doc(hidden)]
pub trait PlayerDiscoveryProvider: Send + Sync {
    fn discover_players(&self) -> impl std::future::Future<Output = Result<Vec<String>>> + Send;
    fn get_player_info(
        &self,
        player_name: &str,
    ) -> impl std::future::Future<Output = Result<PlayerInfo>> + Send;
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static;
}

/// MPRIS-based provider for Linux.
///
/// This provider uses the MPRIS D-Bus interface to discover and query media players.
#[doc(hidden)]
pub struct MprisProvider {}

impl MprisProvider {
    #[doc(hidden)]
    pub fn new() -> Result<Self> {
        // Verify that we can create a PlayerFinder (connection is available)
        PlayerFinder::new().map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        Ok(Self {})
    }
}

impl PlayerDiscoveryProvider for MprisProvider {
    async fn discover_players(&self) -> Result<Vec<String>> {
        let finder =
            PlayerFinder::new().map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        let players = finder
            .find_all()
            .map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        let player_names: Vec<String> = players
            .iter()
            .map(|p| extract_player_name(p.bus_name()))
            .collect();

        Ok(player_names)
    }

    async fn get_player_info(&self, player_name: &str) -> Result<PlayerInfo> {
        let finder =
            PlayerFinder::new().map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        let players = finder
            .find_all()
            .map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        let player = players
            .iter()
            .find(|p| extract_player_name(p.bus_name()) == player_name)
            .ok_or_else(|| MediaWatcherError::PlayerNotFound(player_name.to_string()))?;

        let metadata = player
            .get_metadata()
            .map_err(|e| MediaWatcherError::ParseError(e.to_string()))?;

        let playback_status = player
            .get_playback_status()
            .map_err(|e| MediaWatcherError::ParseError(e.to_string()))?;

        let position = player.get_position().ok();

        let volume = player.get_volume().ok();

        let current_track =
            if metadata.title().is_some() || !metadata.artists().unwrap_or(vec![]).is_empty() {
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

    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn a dedicated thread for MPRIS event monitoring
        // We use std::thread because mpris::Player is not Send and cannot be used with tokio::spawn
        std::thread::spawn(move || {
            let mut monitor = PlayerMonitor::new();
            let mut last_check = std::time::Instant::now();

            loop {
                // Check for channel closure
                if tx.is_closed() {
                    break;
                }

                // Poll for player updates every 500ms
                if last_check.elapsed() >= Duration::from_millis(500) {
                    // Create a new finder instance for each iteration
                    let Ok(finder) = PlayerFinder::new() else {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    };

                    let Ok(current_players) = finder.find_all() else {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    };

                    // Process players and get events
                    let events = monitor.process_players(current_players);

                    // Send all events
                    for event in events {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }

                    last_check = std::time::Instant::now();
                }

                // Small sleep to avoid busy waiting
                std::thread::sleep(Duration::from_millis(100));
            }
        });

        UnboundedReceiverStream::new(rx)
    }
}

/// Linux media watcher implementation using MPRIS D-Bus interface.
///
/// Note: This type is visible for technical reasons but should not be used directly.
/// Use `MediaWatcherBuilder` to create media watchers.
pub struct LinuxMediaWatcher<P: PlayerDiscoveryProvider = MprisProvider> {
    provider: Arc<P>,
}

struct PlayerMonitor {
    known_players: HashMap<String, Player>,
    last_states: HashMap<String, PlayerState>,
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct PlayerState {
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

    #[cfg(test)]
    const fn known_players(&self) -> &HashMap<String, Player> {
        &self.known_players
    }

    fn process_players(&mut self, current_players: Vec<Player>) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        let mut current_names = HashMap::new();

        // Process each player and detect changes
        for player in current_players {
            let player_name = extract_player_name(player.bus_name());

            // Detect new players
            if !self.known_players.contains_key(&player_name) {
                events.push(MediaEvent::PlayerAdded {
                    player_name: player_name.clone(),
                });
            }

            // Get current state and detect changes
            if let Some(current_state) = Self::get_player_state(&player) {
                self.detect_state_changes(&player_name, &current_state, &mut events);
                self.last_states.insert(player_name.clone(), current_state);
            }

            current_names.insert(player_name, player);
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

    fn get_player_state(player: &Player) -> Option<PlayerState> {
        let metadata = player.get_metadata().ok()?;
        let playback_status = player.get_playback_status().ok()?;

        let track =
            if metadata.title().is_some() || !metadata.artists().unwrap_or(vec![]).is_empty() {
                Some(parse_metadata(&metadata))
            } else {
                None
            };

        let position = player.get_position().ok();

        let volume = player.get_volume().ok();

        Some(PlayerState {
            track,
            playback_state: parse_playback_status(playback_status),
            position,
            volume,
        })
    }

    fn detect_state_changes(
        &self,
        player_name: &str,
        current: &PlayerState,
        events: &mut Vec<MediaEvent>,
    ) {
        match self.last_states.get(player_name) {
            Some(last) => {
                // Check for track changes
                if current.track != last.track
                    && let Some(current_track) = &current.track
                {
                    events.push(MediaEvent::TrackChanged {
                        player_name: player_name.to_string(),
                        track: current_track.clone(),
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
                if current.volume != last.volume
                    && let Some(volume) = current.volume
                {
                    events.push(MediaEvent::VolumeChanged {
                        player_name: player_name.to_string(),
                        volume,
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

    fn detect_position_change(
        player_name: &str,
        current: &PlayerState,
        last: &PlayerState,
        events: &mut Vec<MediaEvent>,
    ) {
        if let (Some(pos), Some(last_pos)) = (current.position, last.position) {
            // If difference is more than 2 seconds, it's probably a seek
            let diff = pos.abs_diff(last_pos);
            if diff > Duration::from_secs(2) {
                events.push(MediaEvent::PositionChanged {
                    player_name: player_name.to_string(),
                    position: pos,
                });
            }
        }
    }
}

impl LinuxMediaWatcher<MprisProvider> {
    /// Creates a new Linux media watcher.
    ///
    /// Note: This is an internal API. Use `MediaWatcherBuilder` instead.
    #[doc(hidden)]
    pub fn new() -> Result<Self> {
        Ok(Self {
            provider: Arc::new(MprisProvider::new()?),
        })
    }
}

impl<P: PlayerDiscoveryProvider + 'static> LinuxMediaWatcher<P> {
    #[cfg(test)]
    pub const fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

impl<P: PlayerDiscoveryProvider + 'static> MediaWatcher for LinuxMediaWatcher<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.provider.discover_players().await
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        self.provider.get_player_info(player_name).await
    }

    async fn event_stream(&self) -> Result<EventStream<'static>> {
        let stream = self.provider.create_event_stream();
        Ok(Box::pin(stream))
    }
}

// Helper functions

fn extract_player_name(bus_name: &str) -> String {
    bus_name
        .strip_prefix("org.mpris.MediaPlayer2.")
        .unwrap_or(bus_name)
        .to_string()
}

#[allow(clippy::cast_sign_loss)]
fn parse_metadata(metadata: &Metadata) -> Track {
    let title = metadata
        .title()
        .map_or_else(|| "Unknown".to_string(), ToString::to_string);

    let artist = metadata
        .artists()
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();

    let album = metadata.album_name().map(ToString::to_string);

    let album_artist = metadata
        .album_artists()
        .map(|artists| artists.iter().map(ToString::to_string).collect());

    let track_number = metadata.track_number().map(|number| number as u32);

    let duration = metadata.length();

    let art_url = metadata.art_url().map(ToString::to_string);

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

const fn parse_playback_status(status: MprisPlaybackStatus) -> PlaybackState {
    match status {
        MprisPlaybackStatus::Playing => PlaybackState::Playing,
        MprisPlaybackStatus::Paused => PlaybackState::Paused,
        MprisPlaybackStatus::Stopped => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpris::MetadataValue;
    use std::collections::HashMap;

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
                .ok_or_else(|| MediaWatcherError::PlayerNotFound(player_name.to_string()))
        }

        fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
            futures::stream::empty()
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
            album_artist: None,
            track_number: None,
            duration: Some(Duration::from_secs(180)),
            art_url: None,
        }
    }

    // LinuxMediaWatcher tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_players() -> Result<()> {
        let provider = Arc::new(MockPlayerDiscoveryProvider::new());
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await?;
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
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let players = watcher.list_players().await?;
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
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let mut players = watcher.list_players().await?;
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
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("spotify").await?;
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
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let result = watcher.get_player("nonexistent").await;
        assert_eq!(
            result,
            Err(MediaWatcherError::PlayerNotFound("nonexistent".to_string()))
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
        let watcher = LinuxMediaWatcher::with_provider(provider);

        let player_info = watcher.get_player("vlc").await?;
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

    // Helper function to create Metadata for testing
    fn create_metadata(values: HashMap<String, MetadataValue>) -> Metadata {
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
            MetadataValue::String("Test Song".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String("Test Artist".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            MetadataValue::String("Test Album".to_string()),
        );
        values.insert(
            "xesam:albumArtist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String("Album Artist".to_string())]),
        );
        values.insert("xesam:trackNumber".to_string(), MetadataValue::I32(5));
        values.insert("mpris:length".to_string(), MetadataValue::I64(180_000_000)); // microseconds
        values.insert(
            "mpris:artUrl".to_string(),
            MetadataValue::String("file:///path/to/art.jpg".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Test Song".to_string(),
                artist: vec!["Test Artist".to_string()],
                album: Some("Test Album".to_string()),
                album_artist: Some(vec!["Album Artist".to_string()]),
                track_number: Some(5),
                duration: Some(Duration::from_secs(180)),
                art_url: Some("file:///path/to/art.jpg".to_string()),
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_multiple_artists() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            MetadataValue::String("Collaboration Song".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![
                MetadataValue::String("Artist 1".to_string()),
                MetadataValue::String("Artist 2".to_string()),
                MetadataValue::String("Artist 3".to_string()),
            ]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Collaboration Song".to_string(),
                artist: vec![
                    "Artist 1".to_string(),
                    "Artist 2".to_string(),
                    "Artist 3".to_string()
                ],
                album: None,
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_minimal_info() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            MetadataValue::String("Minimal Song".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Minimal Song".to_string(),
                artist: Vec::<String>::new(),
                album: None,
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_without_title() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String("Artist Only".to_string())]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Unknown".to_string(),
                artist: vec!["Artist Only".to_string()],
                album: None,
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_empty_metadata() {
        let values = HashMap::new();
        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Unknown".to_string(),
                artist: Vec::<String>::new(),
                album: None,
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_unicode() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            MetadataValue::String("テスト曲".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String("アーティスト名".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            MetadataValue::String("アルバム🎵".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "テスト曲".to_string(),
                artist: vec!["アーティスト名".to_string()],
                album: Some("アルバム🎵".to_string()),
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_special_characters() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            MetadataValue::String("Song: The \"Best\" & Greatest (2024)".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String(
                "Artist's Name / Band".to_string(),
            )]),
        );
        values.insert(
            "xesam:album".to_string(),
            MetadataValue::String("Album <Special Edition>".to_string()),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Song: The \"Best\" & Greatest (2024)".to_string(),
                artist: vec!["Artist's Name / Band".to_string()],
                album: Some("Album <Special Edition>".to_string()),
                album_artist: None,
                track_number: None,
                duration: None,
                art_url: None,
            }
        );
    }

    #[test]
    fn test_parse_metadata_with_multiple_album_artists() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title".to_string(),
            MetadataValue::String("Compilation Track".to_string()),
        );
        values.insert(
            "xesam:artist".to_string(),
            MetadataValue::Array(vec![MetadataValue::String("Track Artist".to_string())]),
        );
        values.insert(
            "xesam:album".to_string(),
            MetadataValue::String("Various Artists".to_string()),
        );
        values.insert(
            "xesam:albumArtist".to_string(),
            MetadataValue::Array(vec![
                MetadataValue::String("Artist A".to_string()),
                MetadataValue::String("Artist B".to_string()),
            ]),
        );

        let metadata = create_metadata(values);
        let track = parse_metadata(&metadata);

        assert_eq!(
            track,
            Track {
                title: "Compilation Track".to_string(),
                artist: vec!["Track Artist".to_string()],
                album: Some("Various Artists".to_string()),
                album_artist: Some(vec!["Artist A".to_string(), "Artist B".to_string()]),
                track_number: None,
                duration: None,
                art_url: None,
            }
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

        let track = create_test_track("Song 1");
        let state = create_test_state(
            Some(track.clone()),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );

        monitor.detect_state_changes("spotify", &state, &mut events);

        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "spotify".to_string(),
                    track
                },
                MediaEvent::StateChanged {
                    player_name: "spotify".to_string(),
                    state: PlaybackState::Playing
                }
            ]
        );
    }

    #[test]
    fn test_detect_state_changes_first_time_without_track() {
        let monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        let state = create_test_state(None, PlaybackState::Stopped, None, None);

        monitor.detect_state_changes("spotify", &state, &mut events);

        // No events should be generated for a player with no track
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_detect_state_changes_track_changed() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let track1 = create_test_track("Song 1");
        let initial_state = create_test_state(
            Some(track1),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with different track
        let track2 = create_test_track("Song 2");
        let new_state = create_test_state(
            Some(track2.clone()),
            PlaybackState::Playing,
            Some(Duration::from_secs(5)),
            Some(0.8),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "spotify".to_string(),
                    track: track2
                },
                MediaEvent::PositionChanged {
                    player_name: "spotify".to_string(),
                    position: Duration::from_secs(5)
                }
            ]
        );
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

        assert_eq!(
            events,
            vec![MediaEvent::StateChanged {
                player_name: "spotify".to_string(),
                state: PlaybackState::Paused
            }]
        );
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

        assert_eq!(
            events,
            vec![MediaEvent::VolumeChanged {
                player_name: "spotify".to_string(),
                volume: 0.5
            }]
        );
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
        assert_eq!(events, Vec::<MediaEvent>::new());
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

        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "spotify".to_string(),
                position: Duration::from_secs(60),
            }]
        );
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

        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "spotify".to_string(),
                position: Duration::from_secs(10),
            }]
        );
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
        assert_eq!(events, Vec::<MediaEvent>::new());
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
        assert_eq!(events, Vec::<MediaEvent>::new());
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
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_detect_state_changes_multiple_changes() {
        let mut monitor = PlayerMonitor::new();
        let mut events = Vec::new();

        // Initial state
        let track1 = create_test_track("Song 1");
        let initial_state = create_test_state(
            Some(track1),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
            Some(0.8),
        );
        monitor
            .last_states
            .insert("spotify".to_string(), initial_state);

        // New state with multiple changes
        let track2 = create_test_track("Song 2");
        let new_state = create_test_state(
            Some(track2.clone()),
            PlaybackState::Paused,
            Some(Duration::from_secs(70)), // Significant position change
            Some(0.5),
        );

        monitor.detect_state_changes("spotify", &new_state, &mut events);

        // Should detect: track change, state change, position change, volume change
        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "spotify".to_string(),
                    track: track2,
                },
                MediaEvent::StateChanged {
                    player_name: "spotify".to_string(),
                    state: PlaybackState::Paused,
                },
                MediaEvent::PositionChanged {
                    player_name: "spotify".to_string(),
                    position: Duration::from_secs(70),
                },
                MediaEvent::VolumeChanged {
                    player_name: "spotify".to_string(),
                    volume: 0.5,
                }
            ]
        );
    }
}
