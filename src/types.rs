//! Core types for media playback information.
//!
//! This module defines the main data structures used to represent media tracks,
//! playback states, player information, and events emitted by media players.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Album artwork for a [`Track`].
///
/// Artwork is delivered in one of two forms depending on the platform and player:
/// a URI that points to the image ([`Artwork::Url`]), or the raw image bytes
/// themselves ([`Artwork::Bytes`]). Linux (MPRIS) and macOS report a URI, while
/// Windows exposes the thumbnail as binary data.
///
/// Use [`Artwork::to_uri`] when you just need a single string to render (for
/// example, an `<img>` source) regardless of the underlying form.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
pub enum Artwork {
    /// Artwork available at a URI (`http(s):`, `file:`, or `data:`).
    // NOTE: `Url` is a struct variant, not `Url(String)`, on purpose. serde's
    // internally tagged form (`tag = "type"`, kept consistent with `MediaEvent`)
    // cannot encode a newtype variant that wraps a plain `String`, so the field
    // is named. Do not "simplify" this to a tuple/newtype variant.
    Url {
        /// The artwork URI.
        url: String,
    },
    /// Raw artwork image bytes.
    Bytes {
        /// MIME type of the image (e.g. `image/jpeg`), if the player reported one.
        mime: Option<String>,
        /// Raw image bytes, serialized as a base64 string.
        ///
        /// Wrapped in [`Arc`] so cloning a [`Track`] (which happens on every
        /// emitted event) does not copy the image data.
        #[cfg_attr(feature = "serde", serde(with = "artwork_bytes"))]
        data: Arc<[u8]>,
    },
}

impl Artwork {
    /// MIME type used by [`Self::to_uri`] for [`Self::Bytes`] when the player
    /// did not report one.
    const DEFAULT_MIME: &str = "application/octet-stream";

    /// Returns the artwork URI when this is [`Artwork::Url`], otherwise `None`.
    #[must_use]
    pub fn as_url(&self) -> Option<&str> {
        match self {
            Self::Url { url } => Some(url),
            Self::Bytes { .. } => None,
        }
    }

    /// Returns the raw image bytes when this is [`Artwork::Bytes`], otherwise `None`.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes { data, .. } => Some(data),
            Self::Url { .. } => None,
        }
    }

    /// Returns the MIME type when this is [`Artwork::Bytes`] and one was reported.
    #[must_use]
    pub fn mime(&self) -> Option<&str> {
        match self {
            Self::Bytes { mime, .. } => mime.as_deref(),
            Self::Url { .. } => None,
        }
    }

    /// Returns a URI that can be used directly, e.g. as an `<img>` source.
    ///
    /// For [`Artwork::Url`] the stored URI is borrowed as-is. For
    /// [`Artwork::Bytes`] a `data:<mime>;base64,<data>` URI is built (allocating
    /// and base64-encoding); the MIME type falls back to
    /// `application/octet-stream` when unknown.
    #[must_use]
    pub fn to_uri(&self) -> Cow<'_, str> {
        match self {
            Self::Url { url } => Cow::Borrowed(url),
            Self::Bytes { mime, data } => {
                let mime = mime.as_deref().unwrap_or(Self::DEFAULT_MIME);
                let bytes: &[u8] = data;
                Cow::Owned(format!("data:{mime};base64,{}", BASE64.encode(bytes)))
            }
        }
    }
}

/// `serde` (de)serialization for [`Artwork::Bytes`] image data as a base64 string.
///
/// JSON has no native binary type, so the in-memory `Arc<[u8]>` is encoded as
/// base64 at the serialization boundary (and decoded back on the way in).
#[cfg(feature = "serde")]
mod artwork_bytes {
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes: &[u8] = value;
        serializer.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = BASE64
            .decode(encoded.as_bytes())
            .map_err(D::Error::custom)?;
        Ok(Arc::from(bytes))
    }
}

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
///     artwork: None,
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
    /// Album artwork, if available.
    ///
    /// Linux and macOS report a URI ([`Artwork::Url`]); Windows reports the raw
    /// thumbnail bytes ([`Artwork::Bytes`]). Use [`Artwork::to_uri`] to obtain a
    /// directly renderable string regardless of the form.
    pub artwork: Option<Artwork>,
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
            artwork: None,
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

#[cfg(test)]
mod artwork_tests {
    use super::*;

    #[test]
    fn url_accessors() {
        let art = Artwork::Url {
            url: "https://example.com/a.jpg".to_string(),
        };
        assert_eq!(art.as_url(), Some("https://example.com/a.jpg"));
        assert_eq!(art.as_bytes(), None);
        assert_eq!(art.mime(), None);
    }

    #[test]
    fn bytes_accessors() {
        let art = Artwork::Bytes {
            mime: Some("image/png".to_string()),
            data: Arc::from([1u8, 2, 3].as_slice()),
        };
        assert_eq!(art.as_url(), None);
        assert_eq!(art.as_bytes(), Some([1u8, 2, 3].as_slice()));
        assert_eq!(art.mime(), Some("image/png"));
    }

    #[test]
    fn to_uri_borrows_url() {
        let art = Artwork::Url {
            url: "https://example.com/a.jpg".to_string(),
        };
        assert!(matches!(
            art.to_uri(),
            Cow::Borrowed("https://example.com/a.jpg")
        ));
    }

    #[test]
    fn to_uri_builds_data_uri_for_bytes() {
        // "Hi" encodes to "SGk=" in standard base64.
        let art = Artwork::Bytes {
            mime: Some("image/jpeg".to_string()),
            data: Arc::from(b"Hi".as_slice()),
        };
        assert_eq!(art.to_uri(), "data:image/jpeg;base64,SGk=");
    }

    #[test]
    fn to_uri_falls_back_to_default_mime() {
        let art = Artwork::Bytes {
            mime: None,
            data: Arc::from(b"Hi".as_slice()),
        };
        assert_eq!(art.to_uri(), "data:application/octet-stream;base64,SGk=");
    }
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
            artwork: Some(Artwork::Url {
                url: "https://example.com/art.jpg".to_string(),
            }),
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
        assert_eq!(value["artwork"], json!(null));
    }

    #[test]
    fn artwork_url_is_internally_tagged() {
        let art = Artwork::Url {
            url: "https://example.com/art.jpg".to_string(),
        };
        let value = serde_json::to_value(art).expect("artwork serializes");
        assert_eq!(value["type"], json!("Url"));
        assert_eq!(value["url"], json!("https://example.com/art.jpg"));
    }

    #[test]
    fn artwork_bytes_serializes_data_as_base64() {
        let art = Artwork::Bytes {
            mime: Some("image/jpeg".to_string()),
            data: Arc::from(b"Hi".as_slice()),
        };
        let value = serde_json::to_value(art).expect("artwork serializes");
        assert_eq!(value["type"], json!("Bytes"));
        assert_eq!(value["mime"], json!("image/jpeg"));
        // "Hi" encodes to "SGk=" in standard base64.
        assert_eq!(value["data"], json!("SGk="));
    }

    #[test]
    fn artwork_bytes_round_trips() {
        let art = Artwork::Bytes {
            mime: None,
            data: Arc::from([0u8, 1, 2, 3].as_slice()),
        };
        let json = serde_json::to_string(&art).expect("artwork serializes");
        let back: Artwork = serde_json::from_str(&json).expect("artwork deserializes");
        assert_eq!(art, back);
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
