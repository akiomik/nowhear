//! Core types for media playback information.
//!
//! This module defines the main data structures used to represent media tracks,
//! playback states, player information, and events emitted by media players.

use std::time::Duration;

/// `serde` (de)serialization for a required [`Duration`] as integer milliseconds.
///
/// Kept separate from [`duration_millis_opt`] because `#[serde(with = ...)]` must match the
/// exact field type, and the optional variant cannot be reused for a bare `Duration`.
#[cfg(feature = "serde")]
mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Tracks never run long enough to overflow u64 milliseconds; saturate defensively.
        serializer.serialize_u64(u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        u64::deserialize(deserializer).map(Duration::from_millis)
    }
}

/// `serde` (de)serialization for an optional [`Duration`] as integer milliseconds (or `null`).
#[cfg(feature = "serde")]
mod duration_millis_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::ref_option)]
    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => serializer.serialize_some(&u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_millis))
    }
}

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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    /// Total duration of the track, serialized as integer milliseconds.
    #[cfg_attr(feature = "serde", serde(with = "duration_millis_opt"))]
    pub duration: Option<Duration>,
    /// URI of the album artwork, if available.
    ///
    /// This is always a URI rather than a bare filesystem path. Depending on the
    /// platform and player it may be an `http(s):`, `file:`, or `data:` URI — for
    /// example, the Windows backend inlines the thumbnail bytes as a
    /// `data:<mime>;base64,<data>` URI.
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlayerInfo {
    /// Name or identifier of the player
    pub player_name: String,
    /// Currently playing track, if any
    pub current_track: Option<Track>,
    /// Current playback state (playing, paused, or stopped)
    pub playback_state: PlaybackState,
    /// Current playback position within the track, serialized as integer milliseconds.
    #[cfg_attr(feature = "serde", serde(with = "duration_millis_opt"))]
    pub position: Option<Duration>,
    /// Current volume level, where `0.0` is muted and `1.0` is full volume.
    ///
    /// Values may exceed `1.0` on players that support over-amplification (the
    /// MPRIS `Volume` property is not capped at `1.0`).
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
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
        #[cfg_attr(feature = "serde", serde(with = "duration_millis"))]
        position: Duration,
    },

    /// Volume changed.
    ///
    /// This event is emitted when the player's volume level changes.
    /// The volume value is `0.0` (muted) to `1.0` (full volume), and may exceed
    /// `1.0` on players that support over-amplification (e.g. via MPRIS).
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

/// Tests that lock the public 1.0 serde wire format. Changing any assertion here is a
/// breaking change for consumers that persist or exchange the JSON representation.
#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;

    use serde_json::json;

    fn sample_track() -> Track {
        Track {
            title: "Bohemian Rhapsody".to_string(),
            artist: vec!["Queen".to_string()],
            album: Some("A Night at the Opera".to_string()),
            album_artist: vec!["Queen".to_string()],
            track_number: Some(11),
            duration: Some(Duration::from_secs(354)),
            art_url: Some("https://example.com/art.jpg".to_string()),
        }
    }

    #[test]
    fn track_serializes_duration_as_integer_millis() {
        let value = serde_json::to_value(sample_track()).expect("track serializes");
        assert_eq!(value["duration"], json!(354_000));
    }

    #[test]
    fn track_omitting_duration_serializes_null() {
        let track = Track::unknown();
        let value = serde_json::to_value(track).expect("track serializes");
        assert_eq!(value["duration"], json!(null));
        assert_eq!(value["art_url"], json!(null));
    }

    #[test]
    fn track_round_trips() {
        let track = sample_track();
        let json = serde_json::to_string(&track).expect("track serializes");
        let back: Track = serde_json::from_str(&json).expect("track deserializes");
        assert_eq!(track, back);
    }

    #[test]
    fn playback_state_serializes_as_pascal_case() {
        let value = serde_json::to_value(PlaybackState::Playing).expect("state serializes");
        assert_eq!(value, json!("Playing"));
    }

    #[test]
    fn media_event_is_internally_tagged() {
        let event = MediaEvent::StateChanged {
            player_name: "spotify".to_string(),
            state: PlaybackState::Playing,
        };
        let value = serde_json::to_value(event).expect("event serializes");
        assert_eq!(value["type"], json!("StateChanged"));
        assert_eq!(value["player_name"], json!("spotify"));
        assert_eq!(value["state"], json!("Playing"));
    }

    #[test]
    fn media_event_position_serializes_as_integer_millis() {
        let event = MediaEvent::PositionChanged {
            player_name: "spotify".to_string(),
            position: Duration::from_secs(12),
        };
        let value = serde_json::to_value(event).expect("event serializes");
        assert_eq!(value["type"], json!("PositionChanged"));
        assert_eq!(value["position"], json!(12_000));
    }

    #[test]
    fn media_event_round_trips() {
        let event = MediaEvent::TrackChanged {
            player_name: "spotify".to_string(),
            track: sample_track(),
        };
        let json = serde_json::to_string(&event).expect("event serializes");
        let back: MediaEvent = serde_json::from_str(&json).expect("event deserializes");
        assert_eq!(event, back);
    }
}
