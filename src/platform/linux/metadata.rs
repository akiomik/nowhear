use std::time::Duration;

use zbus::zvariant::{Dict, Value};

use crate::{MediaSourceError, Result, Track};

/// Internal representation of MPRIS metadata.
///
/// This struct maps MPRIS D-Bus metadata fields to Rust types.
/// It is converted to [`Track`] for public API use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MprisMetadata {
    art_url: Option<String>,
    length: Option<u64>,
    track_id: Option<String>,
    album: Option<String>,
    album_artist: Vec<String>,
    artist: Vec<String>,
    auto_rating: Option<f64>,
    disc_number: Option<i32>,
    title: Option<String>,
    track_number: Option<i32>,
    url: Option<String>,
}

impl TryFrom<Dict<'_, '_>> for MprisMetadata {
    type Error = MediaSourceError;

    fn try_from(dict: Dict) -> Result<Self> {
        let mut metadata = Self::default();

        let art_url_key = Value::new("mpris:artUrl");
        if let Some(Value::Str(art_url)) = dict.get(&art_url_key)? {
            let s = art_url.to_string();
            if !s.is_empty() {
                metadata.art_url = Some(s);
            }
        }

        let length_key = Value::new("mpris:length");
        if let Some(Value::U64(length)) = dict.get(&length_key)? {
            metadata.length = Some(length);
        }

        let track_id_key = Value::new("mpris:trackid");
        if let Some(Value::Str(track_id)) = dict.get(&track_id_key)? {
            metadata.track_id = Some(track_id.to_string());
        }

        let album_key = Value::new("xesam:album");
        if let Some(Value::Str(album)) = dict.get(&album_key)? {
            let s = album.to_string();
            if !s.is_empty() {
                metadata.album = Some(s);
            }
        }

        let album_artist_key = Value::new("xesam:albumArtist");
        if let Some(Value::Array(artists)) = dict.get(&album_artist_key)? {
            metadata.album_artist = artists
                .iter()
                .filter_map(|artist| match artist {
                    Value::Str(artist) => {
                        let s = artist.to_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    _ => None,
                })
                .collect();
        }

        let artist_key = Value::new("xesam:artist");
        if let Some(Value::Array(artists)) = dict.get(&artist_key)? {
            metadata.artist = artists
                .iter()
                .filter_map(|artist| match artist {
                    Value::Str(artist) => {
                        let s = artist.to_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    _ => None,
                })
                .collect();
        }

        let auto_rating_key = Value::new("xesam:autoRating");
        if let Some(Value::F64(auto_rating)) = dict.get(&auto_rating_key)? {
            metadata.auto_rating = Some(auto_rating);
        }

        let disc_number_key = Value::new("xesam:discNumber");
        if let Some(Value::I32(disc_number)) = dict.get(&disc_number_key)? {
            metadata.disc_number = Some(disc_number);
        }

        let title_key = Value::new("xesam:title");
        if let Some(Value::Str(title)) = dict.get(&title_key)? {
            metadata.title = Some(title.to_string());
        }

        let track_number_key = Value::new("xesam:trackNumber");
        if let Some(Value::I32(track_number)) = dict.get(&track_number_key)? {
            metadata.track_number = Some(track_number);
        }

        let url_key = Value::new("xesam:url");
        if let Some(Value::Str(url)) = dict.get(&url_key)? {
            metadata.url = Some(url.to_string());
        }

        Ok(metadata)
    }
}

impl From<MprisMetadata> for Track {
    fn from(metadata: MprisMetadata) -> Self {
        let mut track = Self::unknown();
        if let Some(title) = metadata.title {
            track.title = title;
        }

        track.artist = metadata.artist;
        track.album = metadata.album;
        track.album_artist = metadata.album_artist;
        track.track_number = metadata.track_number.and_then(|num| num.try_into().ok());
        track.duration = metadata.length.map(Duration::from_micros);
        track.art_url = metadata.art_url;

        track
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    #[test]
    fn test_mpris_metadata_try_from_dict_with_full_info() {
        let mut values = HashMap::new();
        values.insert("mpris:artUrl", Value::new("file:///path/to/art.jpg"));
        values.insert("mpris:length", Value::U64(180_000_000)); // microseconds
        values.insert("mpris:trackid", Value::new("/com/spotify/track/deadbeef"));
        values.insert("xesam:album", Value::new("Test Album"));
        values.insert("xesam:albumArtist", Value::new(vec!["Album Artist"]));
        values.insert("xesam:artist", Value::new(vec!["Test Artist"]));
        values.insert("xesam:autoRating", Value::new(0.33));
        values.insert("xesam:discNumber", Value::I32(1));
        values.insert("xesam:title", Value::new("Test Song"));
        values.insert("xesam:trackNumber", Value::I32(5));
        values.insert("xesam:url", Value::new("https://example.com"));
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                art_url: Some("file:///path/to/art.jpg".to_owned()),
                length: Some(180_000_000), // microseconds
                track_id: Some("/com/spotify/track/deadbeef".to_owned()),
                album: Some("Test Album".to_owned()),
                album_artist: vec!["Album Artist".to_owned()],
                artist: vec!["Test Artist".to_owned()],
                auto_rating: Some(0.33),
                disc_number: Some(1),
                title: Some("Test Song".to_owned()),
                track_number: Some(5),
                url: Some("https://example.com".to_owned()),
            })
        );
    }

    #[test]
    fn test_parse_metadata_with_multiple_artists() {
        let mut values = HashMap::new();
        values.insert("xesam:title", Value::new("Collaboration Song"));
        values.insert(
            "xesam:artist",
            Value::new(vec!["Artist 1", "Artist 2", "Artist 3"]),
        );
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("Collaboration Song".to_owned()),
                artist: vec![
                    "Artist 1".to_string(),
                    "Artist 2".to_string(),
                    "Artist 3".to_string()
                ],
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_parse_metadata_with_minimal_info() {
        let mut values = HashMap::new();
        values.insert("xesam:title", "Minimal Song");
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("Minimal Song".to_owned()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_parse_metadata_without_title() {
        let mut values = HashMap::new();
        values.insert("xesam:artist", vec!["Artist Only"]);
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: None,
                artist: vec!["Artist Only".to_owned()],
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_parse_metadata_with_empty_metadata() {
        let values: HashMap<String, Value> = HashMap::new();
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(metadata, Ok(MprisMetadata::default()));
    }

    #[test]
    fn test_parse_metadata_with_unicode() {
        let mut values = HashMap::new();
        values.insert("xesam:title", Value::new("テスト曲"));
        values.insert("xesam:artist", Value::new(vec!["アーティスト名"]));
        values.insert("xesam:album", Value::new("アルバム🎵"));
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("テスト曲".to_string()),
                artist: vec!["アーティスト名".to_string()],
                album: Some("アルバム🎵".to_string()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_parse_metadata_with_special_characters() {
        let mut values = HashMap::new();
        values.insert(
            "xesam:title",
            Value::new("Song: The \"Best\" & Greatest (2024)"),
        );
        values.insert("xesam:artist", Value::new(vec!["Artist's Name / Band"]));
        values.insert("xesam:album", Value::new("Album <Special Edition>"));
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("Song: The \"Best\" & Greatest (2024)".to_owned()),
                artist: vec!["Artist's Name / Band".to_string()],
                album: Some("Album <Special Edition>".to_string()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_parse_metadata_with_multiple_album_artists() {
        let mut values = HashMap::new();
        values.insert("xesam:title", Value::new("Compilation Track"));
        values.insert("xesam:artist", Value::new(vec!["Track Artist"]));
        values.insert("xesam:album", Value::new("Various Artists"));
        values.insert(
            "xesam:albumArtist",
            Value::new(vec!["Artist A", "Artist B"]),
        );
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("Compilation Track".to_string()),
                artist: vec!["Track Artist".to_string()],
                album: Some("Various Artists".to_string()),
                album_artist: vec!["Artist A".to_string(), "Artist B".to_string()],
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_mpris_metadata_from_for_track() {
        let metadata = MprisMetadata {
            title: Some("Test Song".to_owned()),
            artist: vec!["Test Artist".to_owned()],
            album: Some("Test Album".to_owned()),
            album_artist: vec!["Album Artist".to_owned()],
            track_number: Some(5),
            length: Some(180_000_000), // microseconds
            art_url: Some("file:///path/to/art.jpg".to_owned()),
            track_id: None,
            auto_rating: None,
            disc_number: None,
            url: None,
        };
        let track = Track::from(metadata);

        assert_eq!(
            track,
            Track {
                title: "Test Song".to_string(),
                artist: vec!["Test Artist".to_string()],
                album: Some("Test Album".to_string()),
                album_artist: vec!["Album Artist".to_string()],
                track_number: Some(5),
                duration: Some(Duration::from_mins(3)),
                art_url: Some("file:///path/to/art.jpg".to_string()),
            }
        );
    }

    #[test]
    fn test_parse_metadata_filters_empty_strings() {
        let mut values = HashMap::new();
        values.insert("xesam:title", Value::new("Test Song"));
        values.insert("xesam:artist", Value::new(vec!["Artist 1", "", "Artist 2"]));
        values.insert(
            "xesam:albumArtist",
            Value::new(vec!["", "Album Artist", ""]),
        );
        values.insert("xesam:album", Value::new(""));
        values.insert("mpris:artUrl", Value::new(""));
        let dict = Dict::from(values);

        let metadata = MprisMetadata::try_from(dict);

        assert_eq!(
            metadata,
            Ok(MprisMetadata {
                title: Some("Test Song".to_string()),
                artist: vec!["Artist 1".to_string(), "Artist 2".to_string()],
                album_artist: vec!["Album Artist".to_string()],
                album: None,
                art_url: None,
                ..Default::default()
            })
        );
    }
}
