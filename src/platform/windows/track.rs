//! Track and time-value conversions from Windows Media Control data.

use std::time::Duration;

use windows::Foundation::IStringable;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties as WinMediaProperties;
use windows::core::Interface;

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
        .and_then(|thumb| {
            thumb
                .cast::<IStringable>()
                .ok()
                .and_then(|stringable| stringable.ToString().ok())
                .map(|s| s.to_string())
        })
        .filter(|s: &String| !s.is_empty());

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
