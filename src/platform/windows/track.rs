//! Track and time-value conversions from Windows Media Control data.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::executor::block_on;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties as WinMediaProperties;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};

use crate::types::Track;

/// MIME type used when the thumbnail stream does not report a content type.
const DEFAULT_ART_MIME: &str = "application/octet-stream";

pub(super) fn build_track(media_props: &WinMediaProperties, duration: Option<Duration>) -> Track {
    let title = media_props.Title().unwrap_or_default().to_string();

    let artist_hstring = media_props.Artist().unwrap_or_default();
    let artist = if artist_hstring.is_empty() {
        vec![]
    } else {
        let s = artist_hstring.to_string();
        if s.is_empty() { vec![] } else { vec![s] }
    };

    let album_title = media_props.AlbumTitle().ok();
    let album = album_title.map(|s| s.to_string()).filter(|s| !s.is_empty());

    let track_number = media_props
        .TrackNumber()
        .ok()
        .and_then(|n| u32::try_from(n).ok());

    let art_url = media_props
        .Thumbnail()
        .ok()
        .and_then(|thumb| read_thumbnail_data_uri(&thumb));

    Track {
        title: if title.is_empty() {
            "Unknown".to_string()
        } else {
            title
        },
        artist,
        album,
        album_artist: vec![],
        track_number,
        duration,
        art_url,
    }
}

/// Reads a media thumbnail and encodes it as a `data:` URI.
///
/// Windows SMTC exposes artwork as an [`IRandomAccessStreamReference`] over binary image data,
/// not as a URL like Linux (MPRIS) and macOS do. To keep the cross-platform `art_url` contract,
/// the bytes are read in full and inlined as a `data:<mime>;base64,<data>` URI so consumers can
/// render them the same way they would a normal URL.
///
/// The WinRT async stream operations are driven to completion synchronously with
/// [`futures::executor::block_on`] rather than `.await`ed on the surrounding Tokio task: the WinRT
/// stream and reader objects are not `Send`, so holding them across a Tokio await point would make
/// the enclosing future non-`Send` and violate the
/// [`MediaSessionProvider`](super::provider::MediaSessionProvider) trait bound. Blocking confines
/// those objects to a single stack frame instead. The thumbnail is an in-memory stream supplied by
/// the player, so reading it does not block on real I/O. Note that, unlike the metadata fetch, this
/// read is therefore not covered by the session-state timeout.
///
/// Returns `None` for empty or unreadable streams.
fn read_thumbnail_data_uri(thumb: &IRandomAccessStreamReference) -> Option<String> {
    block_on(async {
        let stream = thumb.OpenReadAsync().ok()?.await.ok()?;

        let size = stream.Size().ok()?;
        if size == 0 {
            return None;
        }
        // DataReader::LoadAsync takes a u32 count; bail out rather than truncate oversized streams.
        let size = u32::try_from(size).ok()?;

        let content_type = stream
            .ContentType()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let reader = DataReader::CreateDataReader(&stream).ok()?;
        reader.LoadAsync(size).ok()?.await.ok()?;

        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes).ok()?;

        Some(encode_data_uri(&content_type, &bytes))
    })
}

/// Builds a `data:<mime>;base64,<data>` URI from a content type and raw bytes.
///
/// Falls back to [`DEFAULT_ART_MIME`] when the content type is empty.
fn encode_data_uri(content_type: &str, bytes: &[u8]) -> String {
    let mime = if content_type.is_empty() {
        DEFAULT_ART_MIME
    } else {
        content_type
    };
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

/// Converts a Windows `TimeSpan` tick value (100-nanosecond units, i64) to a `Duration`.
///
/// Returns `None` for negative values (Windows sentinel for unavailable positions) and for
/// values that would overflow `u64` when multiplied by 100 (e.g. `i64::MAX`, which Windows
/// uses as a sentinel for unknown/infinite duration in live streams).
#[allow(clippy::cast_sign_loss)]
pub(super) const fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    if ticks < 0 {
        return None;
    }
    match (ticks as u64).checked_mul(100) {
        Some(nanos) => Some(Duration::from_nanos(nanos)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_data_uri_with_content_type() {
        // "Hi" encodes to "SGk=" in standard base64.
        assert_eq!(
            encode_data_uri("image/jpeg", b"Hi"),
            "data:image/jpeg;base64,SGk="
        );
    }

    #[test]
    fn test_encode_data_uri_empty_content_type_falls_back() {
        assert_eq!(
            encode_data_uri("", b"Hi"),
            format!("data:{DEFAULT_ART_MIME};base64,SGk=")
        );
    }

    #[test]
    fn test_encode_data_uri_empty_bytes() {
        assert_eq!(encode_data_uri("image/png", b""), "data:image/png;base64,");
    }

    #[test]
    fn test_encode_data_uri_is_deterministic() {
        // The same bytes must always produce the same URI so the state differ does not report a
        // spurious TrackChanged for an unchanged track.
        assert_eq!(
            encode_data_uri("image/png", b"\x00\x01\x02\x03"),
            encode_data_uri("image/png", b"\x00\x01\x02\x03")
        );
    }

    #[test]
    fn test_ticks_to_duration_negative_is_none() {
        assert_eq!(ticks_to_duration(-1), None);
        assert_eq!(ticks_to_duration(i64::MIN), None);
    }

    #[test]
    fn test_ticks_to_duration_overflow_is_none() {
        // i64::MAX is used by Windows as a sentinel for unknown/infinite duration (live streams).
        // (i64::MAX as u64) * 100 overflows u64::MAX, so the result must be None.
        assert_eq!(ticks_to_duration(i64::MAX), None);
    }

    #[test]
    fn test_ticks_to_duration_zero() {
        assert_eq!(ticks_to_duration(0), Some(Duration::ZERO));
    }

    #[test]
    fn test_ticks_to_duration_one_second() {
        // 1 second = 10_000_000 ticks (100ns units)
        assert_eq!(ticks_to_duration(10_000_000), Some(Duration::from_secs(1)));
    }

    #[test]
    fn test_ticks_to_duration_fractional() {
        // 5_000_000 ticks = 500ms
        assert_eq!(
            ticks_to_duration(5_000_000),
            Some(Duration::from_millis(500))
        );
    }
}
