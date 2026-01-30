//! Core types for media playback information.
//!
//! This module defines the main data structures used to represent media tracks,
//! playback states, player information, and events emitted by media players.

use std::time::Duration;

/// Represents a media track with metadata.
///
/// This structure contains all available metadata about a track, including basic
/// information like title and artist, as well as optional metadata such as album
/// information, track number, duration, and artwork URL.
///
/// # Examples
///
/// ```
/// use nowhear::Track;
/// use std::time::Duration;
///
/// let track = Track {
///     title: "Bohemian Rhapsody".to_string(),
///     artist: vec!["Queen".to_string()],
///     album: Some("A Night at the Opera".to_string()),
///     album_artist: vec!["Queen".to_string()],
///     track_number: Some(11),
///     duration: Some(Duration::from_secs(354)),
///     art_url: None,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Track title
    pub title: String,
    /// List of artists for this track
    pub artist: Vec<String>,
    /// Album name, if available
    pub album: Option<String>,
    /// Album artists, if different from track artists
    pub album_artist: Vec<String>,
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
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            title: "Unknown".to_string(),
            artist: vec![],
            album: None,
            album_artist: vec![],
            track_number: None,
            duration: None,
            art_url: None,
        }
    }
}

/// Playback state of a media player.
///
/// Represents the current playback state of a media player. This is used in
/// both [`PlayerInfo`] and [`MediaEvent::StateChanged`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Media is currently playing.
    Playing,
    /// Media is paused.
    ///
    /// The player has a track loaded but playback is temporarily stopped.
    Paused,
    /// Media is stopped or no media is loaded.
    ///
    /// The player is idle or has no track loaded.
    Stopped,
}

/// Complete information about a media player's current state.
///
/// This structure contains comprehensive information about a media player's current state,
/// including the currently playing track, playback state, position, and volume.
///
/// # Examples
///
/// ```no_run
/// use nowhear::{MediaSource, MediaSourceBuilder, Result};
///
/// # async fn example() -> Result<()> {
/// let source = MediaSourceBuilder::new().build().await?;
/// let player_info = source.get_player("spotify").await?;
///
/// if let Some(track) = player_info.current_track {
///     println!("Playing: {} by {}", track.title, track.artist.join(", "));
/// }
/// # Ok(())
/// # }
/// ```
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
    /// This is useful when a player is detected but not currently playing any media.
    /// The returned `PlayerInfo` will have no track, stopped playback state, and
    /// no position or volume information.
    ///
    /// # Arguments
    ///
    /// * `player_name` - The name of the player
    ///
    /// # Examples
    ///
    /// ```
    /// use nowhear::{PlayerInfo, PlaybackState};
    ///
    /// let info = PlayerInfo::empty("spotify");
    /// assert_eq!(info.player_name, "spotify");
    /// assert_eq!(info.current_track, None);
    /// assert_eq!(info.playback_state, PlaybackState::Stopped);
    /// ```
    #[must_use]
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

/// Events emitted by the media source.
///
/// These events are generated when media playback state changes across any
/// monitored player. Subscribe to these events using [`crate::MediaSource::event_stream`].
///
/// # Examples
///
/// ```no_run
/// use nowhear::{MediaSource, MediaSourceBuilder, MediaEvent, Result};
/// use futures::StreamExt;
///
/// # async fn example() -> Result<()> {
/// let source = MediaSourceBuilder::new().build().await?;
/// let mut stream = source.event_stream().await?;
///
/// while let Some(event) = stream.next().await {
///     match event {
///         MediaEvent::TrackChanged { player_name, track } => {
///             println!("{}: Now playing {}", player_name, track.title);
///         }
///         MediaEvent::StateChanged { player_name, state } => {
///             println!("{}: State changed to {:?}", player_name, state);
///         }
///         _ => {}
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum MediaEvent {
    /// A new track started playing.
    ///
    /// This event is emitted when the current track changes to a different track.
    TrackChanged { player_name: String, track: Track },

    /// Playback state changed.
    ///
    /// This event is emitted when the player transitions between playing, paused, or stopped states.
    StateChanged {
        player_name: String,
        state: PlaybackState,
    },

    /// Playback position changed (seek).
    ///
    /// This event is emitted when the user seeks to a different position in the track.
    /// Note: This is not emitted for normal playback progression, only for significant
    /// position changes (typically > 2 seconds).
    PositionChanged {
        player_name: String,
        position: Duration,
    },

    /// Volume changed.
    ///
    /// This event is emitted when the player's volume level changes.
    /// The volume value is typically between 0.0 (muted) and 1.0 (maximum).
    VolumeChanged { player_name: String, volume: f64 },

    /// A new player appeared.
    ///
    /// This event is emitted when a new media player starts and becomes available
    /// for monitoring.
    PlayerAdded { player_name: String },

    /// A player disappeared.
    ///
    /// This event is emitted when a media player stops or becomes unavailable.
    PlayerRemoved { player_name: String },
}
