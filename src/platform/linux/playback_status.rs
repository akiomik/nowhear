use zbus::zvariant::Str;

use crate::{MediaSourceError, PlaybackState, Result};

/// Internal representation of MPRIS playback status.
///
/// This enum maps MPRIS D-Bus playback status values to Rust types.
/// It is converted to [`PlaybackState`] for public API use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MprisPlaybackStatus {
    /// Player is currently playing.
    Playing,
    /// Player is paused.
    Paused,
    /// Player is stopped.
    Stopped,
}

impl<'a> TryFrom<Str<'a>> for MprisPlaybackStatus {
    type Error = MediaSourceError;

    fn try_from(value: Str<'a>) -> Result<Self> {
        Self::try_from(value.to_string().as_ref())
    }
}

impl TryFrom<&str> for MprisPlaybackStatus {
    type Error = MediaSourceError;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "Playing" => Ok(Self::Playing),
            "Paused" => Ok(Self::Paused),
            "Stopped" => Ok(Self::Stopped),
            unknown => Err(MediaSourceError::ParseError(format!(
                "Unknown playback status: {unknown}"
            ))),
        }
    }
}

impl From<MprisPlaybackStatus> for PlaybackState {
    fn from(mpris_status: MprisPlaybackStatus) -> Self {
        match mpris_status {
            MprisPlaybackStatus::Playing => Self::Playing,
            MprisPlaybackStatus::Paused => Self::Paused,
            MprisPlaybackStatus::Stopped => Self::Stopped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpris_playback_status_try_from_str_ref_playing() {
        assert_eq!(
            MprisPlaybackStatus::try_from("Playing"),
            Ok(MprisPlaybackStatus::Playing)
        );
    }

    #[test]
    fn test_mpris_playback_status_try_from_str_ref_paused() {
        assert_eq!(
            MprisPlaybackStatus::try_from("Paused"),
            Ok(MprisPlaybackStatus::Paused)
        );
    }

    #[test]
    fn test_mpris_playback_status_try_from_str_ref_stopped() {
        assert_eq!(
            MprisPlaybackStatus::try_from("Stopped"),
            Ok(MprisPlaybackStatus::Stopped)
        );
    }

    #[test]
    fn test_mpris_playback_status_try_from_str_ref_unknown() {
        assert_eq!(
            MprisPlaybackStatus::try_from("Foo"),
            Err(MediaSourceError::ParseError(
                "Unknown playback status: Foo".to_string()
            ))
        );
    }

    #[test]
    fn test_mpris_playback_status_from_for_playback_state_playing() {
        assert_eq!(
            PlaybackState::from(MprisPlaybackStatus::Playing),
            PlaybackState::Playing
        );
    }

    #[test]
    fn test_mpris_playback_status_from_for_playback_state_paused() {
        assert_eq!(
            PlaybackState::from(MprisPlaybackStatus::Paused),
            PlaybackState::Paused
        );
    }

    #[test]
    fn test_mpris_playback_status_from_for_playback_state_stopped() {
        assert_eq!(
            PlaybackState::from(MprisPlaybackStatus::Stopped),
            PlaybackState::Stopped
        );
    }
}
