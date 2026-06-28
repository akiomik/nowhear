//! Track and time-value conversions from Windows Media Control data.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::executor::block_on;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties as WinMediaProperties;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::core::Error as WindowsError;

use crate::types::Track;

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
        .and_then(|thumb| Thumbnail::try_from(thumb).ok())
        .and_then(Thumbnail::into_art_url);

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

/// A media thumbnail decoded from Windows Media Control: the raw image bytes plus the reported
/// content type.
///
/// Windows SMTC exposes artwork as an [`IRandomAccessStreamReference`] over binary image data, not
/// as a URL like Linux (MPRIS) and macOS do. Reading is separated from encoding so that the read
/// (`TryFrom`, which touches WinRT) and the `data:` URI formatting ([`into_art_url`]) can be
/// reasoned about — and tested — independently.
///
/// [`into_art_url`]: Self::into_art_url
struct Thumbnail {
    data: Vec<u8>,
    content_type: Option<String>,
}

impl Thumbnail {
    /// MIME type used when the thumbnail stream does not report a content type.
    const DEFAULT_MIME: &str = "application/octet-stream";

    /// Converts the thumbnail into a [`Track::art_url`](crate::types::Track::art_url) value: a
    /// `data:<mime>;base64,<data>` URI that consumers can render the same way they would a normal
    /// URL.
    ///
    /// Falls back to [`Self::DEFAULT_MIME`] when no content type was reported. Returns `None` when
    /// there are no bytes, so an empty thumbnail surfaces as an absent `art_url`.
    fn into_art_url(self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }

        let mime = self
            .content_type
            .unwrap_or_else(|| Self::DEFAULT_MIME.to_string());
        Some(format!("data:{mime};base64,{}", BASE64.encode(self.data)))
    }
}

impl TryFrom<IRandomAccessStreamReference> for Thumbnail {
    type Error = WindowsError;

    /// Reads the thumbnail stream in full.
    ///
    /// The WinRT async stream operations are driven to completion synchronously with
    /// [`futures::executor::block_on`] rather than `.await`ed on the surrounding Tokio task: the
    /// WinRT stream and reader objects are not `Send`, so holding them across a Tokio await point
    /// would make the enclosing future non-`Send` and violate the
    /// [`MediaSessionProvider`](super::provider::MediaSessionProvider) trait bound. Blocking
    /// confines those objects to a single stack frame instead. The thumbnail is an in-memory stream
    /// supplied by the player, so reading it does not block on real I/O. Note that, unlike the
    /// metadata fetch, this read is therefore not covered by the session-state timeout.
    fn try_from(stream_ref: IRandomAccessStreamReference) -> Result<Self, Self::Error> {
        block_on(async {
            let stream = stream_ref.OpenReadAsync()?.await?;

            // DataReader::LoadAsync takes a u32 count. An oversized stream cannot be a real
            // thumbnail; treat it as empty rather than truncating it.
            let Ok(size) = u32::try_from(stream.Size()?) else {
                return Ok(Self {
                    data: Vec::new(),
                    content_type: None,
                });
            };

            let content_type = stream
                .ContentType()
                .ok()
                .map(|ct| ct.to_string())
                .filter(|s| !s.is_empty());

            let reader = DataReader::CreateDataReader(&stream)?;
            reader.LoadAsync(size)?.await?;

            let mut data = vec![0u8; size as usize];
            reader.ReadBytes(&mut data)?;

            Ok(Self { data, content_type })
        })
    }
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

    fn thumbnail(data: &[u8], content_type: Option<&str>) -> Thumbnail {
        Thumbnail {
            data: data.to_vec(),
            content_type: content_type.map(str::to_string),
        }
    }

    #[test]
    fn test_into_art_url_with_content_type() {
        // "Hi" encodes to "SGk=" in standard base64.
        assert_eq!(
            thumbnail(b"Hi", Some("image/jpeg")).into_art_url(),
            Some("data:image/jpeg;base64,SGk=".to_string())
        );
    }

    #[test]
    fn test_into_art_url_without_content_type_falls_back() {
        assert_eq!(
            thumbnail(b"Hi", None).into_art_url(),
            Some(format!("data:{};base64,SGk=", Thumbnail::DEFAULT_MIME))
        );
    }

    #[test]
    fn test_into_art_url_empty_data_is_none() {
        assert_eq!(thumbnail(b"", Some("image/png")).into_art_url(), None);
    }

    #[test]
    fn test_into_art_url_is_deterministic() {
        // The same bytes must always produce the same URI so the state differ does not report a
        // spurious TrackChanged for an unchanged track.
        assert_eq!(
            thumbnail(b"\x00\x01\x02\x03", Some("image/png")).into_art_url(),
            thumbnail(b"\x00\x01\x02\x03", Some("image/png")).into_art_url()
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
