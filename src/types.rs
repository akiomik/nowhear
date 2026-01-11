use std::time::Duration;

/// Represents a media track
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<Vec<String>>,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
    pub art_url: Option<String>,
}

impl Track {
    pub fn unknown() -> Self {
        Self {
            title: "Unknown".to_string(),
            artist: vec![],
            album: None,
            album_artist: None,
            track_number: None,
            duration: None,
            art_url: None,
        }
    }
}

/// Playback state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

/// Player state including track, playback state, position, and volume
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub track: Track,
    pub playback_state: PlaybackState,
    pub position: Option<Duration>,
    pub volume: Option<f64>,
}

/// Player information
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub player_name: String,
    pub current_track: Option<Track>,
    pub playback_state: PlaybackState,
    pub position: Option<Duration>,
    pub volume: Option<f64>, // 0.0 to 1.0
}

impl PlayerInfo {
    pub fn empty(player_name: impl Into<String>) -> Self {
        Self {
            player_name: player_name.into(),
            current_track: None,
            playback_state: PlaybackState::Stopped,
            position: None,
            volume: None,
        }
    }
}

/// Events emitted by the media watcher
#[derive(Debug, Clone, PartialEq)]
pub enum MediaEvent {
    /// A new track started playing
    TrackChanged { player_name: String, track: Track },
    /// Playback state changed
    StateChanged {
        player_name: String,
        state: PlaybackState,
    },
    /// Playback position changed (seek)
    PositionChanged {
        player_name: String,
        position: Duration,
    },
    /// Volume changed
    VolumeChanged { player_name: String, volume: f64 },
    /// A new player appeared
    PlayerAdded { player_name: String },
    /// A player disappeared
    PlayerRemoved { player_name: String },
}
