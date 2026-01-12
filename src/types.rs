use std::time::Duration;

/// Represents a media track with metadata.
///
/// This structure contains all available metadata about a track,
/// such as title, artist, album information, and artwork URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Track title
    pub title: String,
    /// List of artists for this track
    pub artist: Vec<String>,
    /// Album name, if available
    pub album: Option<String>,
    /// Album artists, if different from track artists
    pub album_artist: Option<Vec<String>>,
    /// Track number in the album
    pub track_number: Option<u32>,
    /// Total duration of the track
    pub duration: Option<Duration>,
    /// URL or path to the album artwork
    pub art_url: Option<String>,
}

impl Track {
    /// Creates a track with default "Unknown" values.
    ///
    /// This is useful when track information is not available.
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

/// Playback state of a media player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Media is currently playing
    Playing,
    /// Media is paused
    Paused,
    /// Media is stopped or no media is loaded
    Stopped,
}

/// Internal player state representation.
///
/// This is used internally by platform implementations and is not part of the public API.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlayerState {
    pub(crate) track: Track,
    pub(crate) playback_state: PlaybackState,
    pub(crate) position: Option<Duration>,
    pub(crate) volume: Option<f64>,
}

/// Complete information about a media player's current state.
///
/// This structure contains the player name, current track information,
/// playback state, and other playback details.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    /// Name or identifier of the player
    pub player_name: String,
    /// Currently playing track, if any
    pub current_track: Option<Track>,
    /// Current playback state (playing, paused, or stopped)
    pub playback_state: PlaybackState,
    /// Current playback position within the track
    pub position: Option<Duration>,
    /// Current volume level (0.0 to 1.0)
    pub volume: Option<f64>,
}

impl PlayerInfo {
    /// Creates player info for a player with no active playback.
    ///
    /// # Arguments
    ///
    /// * `player_name` - The name of the player
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
