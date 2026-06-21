//! Playback-status mapping from Windows Media Control to `PlaybackState`.

use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus as WinPlaybackStatus;

use crate::types::PlaybackState;

pub(super) const fn parse_playback_status(status: WinPlaybackStatus) -> PlaybackState {
    match status {
        WinPlaybackStatus::Playing => PlaybackState::Playing,
        WinPlaybackStatus::Paused => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_playback_status_playing() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Playing),
            PlaybackState::Playing
        );
    }

    #[test]
    fn test_parse_playback_status_paused() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Paused),
            PlaybackState::Paused
        );
    }

    #[test]
    fn test_parse_playback_status_stopped() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Stopped),
            PlaybackState::Stopped
        );
    }

    #[test]
    fn test_parse_playback_status_closed() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Closed),
            PlaybackState::Stopped
        );
    }

    #[test]
    fn test_parse_playback_status_changing() {
        assert_eq!(
            parse_playback_status(WinPlaybackStatus::Changing),
            PlaybackState::Stopped
        );
    }
}
