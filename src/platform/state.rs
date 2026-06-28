//! Platform-agnostic player-state snapshot and event diffing.
//!
//! This module has no platform dependencies. It models a single player's observable state
//! (`PlayerState`) and turns successive reads of it into discrete `MediaEvent`s
//! (`diff_player_state`). It is currently consumed only by the Windows backend — which reads a
//! player's full state on every change notification and diffs it against the previous read — but
//! is kept platform-neutral so other backends can adopt it.

use std::time::Duration;

use crate::types::{MediaEvent, PlaybackState, Track};

/// A snapshot of a single player's observable state.
///
/// Successive snapshots are compared by [`diff_player_state`] to derive the events that changed.
/// Not part of the public API.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub track: Track,
    pub playback_state: PlaybackState,
    pub position: Option<Duration>,
    pub volume: Option<f64>,
}

/// Returns `true` when title or artist differs, indicating a genuine track change.
///
/// `Duration`, `artwork`, `track_number`, and `album` are metadata that can be updated late
/// (e.g., a browser loading `EndTime` or album art after buffering) without the song changing.
/// Comparing only `title` and `artist` prevents spurious `TrackChanged` events from those
/// late-loading metadata updates.
fn track_identity_changed(old: Option<&Track>, new: &Track) -> bool {
    old.is_none_or(|o| o.title != new.title || o.artist != new.artist)
}

/// Returns `true` when a position change should be reported as a seek event.
///
/// Reports the first position unconditionally; thereafter only when the jump exceeds 2 seconds,
/// matching the contract in `MediaEvent::PositionChanged` that normal playback progression is not
/// emitted.
fn is_seek(last_position: Option<Duration>, current_position: Duration) -> bool {
    last_position.is_none_or(|last| current_position.abs_diff(last) > Duration::from_secs(2))
}

/// Diffs a freshly-read [`PlayerState`] against the cached one and returns the events to emit.
///
/// A single pure function that turns "the state changed somehow" into the discrete
/// [`MediaEvent`]s that actually differ. Backends that learn of changes without a delta (e.g. the
/// Windows "something changed, re-read everything" notifications) read the full state and call
/// this. Events are ordered `TrackChanged`, `StateChanged`, `PositionChanged`, `VolumeChanged`.
///
/// `old` is `None` only when no prior state is cached. Callers that always emit a baseline on
/// first discovery (as the Windows and macOS backends do) won't pass `None` here.
pub fn diff_player_state(
    player_name: &str,
    old: Option<&PlayerState>,
    new: &PlayerState,
) -> Vec<MediaEvent> {
    let mut events = Vec::new();

    let track_changed = track_identity_changed(old.map(|o| &o.track), &new.track);
    if track_changed {
        events.push(MediaEvent::TrackChanged {
            player_name: player_name.to_string(),
            track: new.track.clone(),
        });
    }

    if old.map(|o| o.playback_state) != Some(new.playback_state) {
        events.push(MediaEvent::StateChanged {
            player_name: player_name.to_string(),
            state: new.playback_state,
        });
    }

    if let Some(position) = new.position {
        // On a genuine track change, treat the position as fresh so the new track's first
        // position is reported. This mirrors the pre-refactor behaviour where a track change
        // reset the cached position to `None` before the timeline handler ran.
        let last = if track_changed {
            None
        } else {
            old.and_then(|o| o.position)
        };
        if is_seek(last, position) {
            events.push(MediaEvent::PositionChanged {
                player_name: player_name.to_string(),
                position,
            });
        }
    }

    // Volume is only diffed against a known previous value; a first-sight baseline (old is None)
    // is emitted by the caller, not here. Backends without a volume notion (e.g. Windows, where it
    // is always None) never produce a VolumeChanged because the value never differs.
    if let Some(old) = old
        && old.volume != new.volume
        && let Some(volume) = new.volume
    {
        events.push(MediaEvent::VolumeChanged {
            player_name: player_name.to_string(),
            volume,
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::Artwork;

    fn create_test_track_for_windows(title: &str) -> Track {
        Track {
            title: title.to_string(),
            artist: vec!["Test Artist".to_string()],
            album: Some("Test Album".to_string()),
            album_artist: vec![],
            track_number: None,
            duration: Some(Duration::from_mins(3)),
            artwork: None,
        }
    }

    fn create_test_state_for_windows(
        track: Track,
        playback_state: PlaybackState,
        position: Option<Duration>,
    ) -> PlayerState {
        PlayerState {
            track,
            playback_state,
            position,
            volume: None,
        }
    }
    // track_identity_changed tests

    #[test]
    fn test_track_identity_changed_no_previous() {
        let track = create_test_track_for_windows("Song");
        assert!(track_identity_changed(None, &track));
    }

    #[test]
    fn test_track_identity_changed_same_identity() {
        let track = create_test_track_for_windows("Song");
        let mut updated = track.clone();
        updated.duration = Some(Duration::from_mins(5)); // duration changed
        updated.artwork = Some(Artwork::Url {
            url: "http://example.com/art.jpg".to_string(),
        });
        assert!(!track_identity_changed(Some(&track), &updated));
    }

    #[test]
    fn test_track_identity_changed_title_changed() {
        let a = create_test_track_for_windows("Song A");
        let b = create_test_track_for_windows("Song B");
        assert!(track_identity_changed(Some(&a), &b));
    }

    #[test]
    fn test_track_identity_changed_album_loaded_late_is_not_change() {
        // Album is late-loading metadata (e.g. a browser populating it after buffering).
        // A change to album alone must not be treated as a genuine track change.
        let track = create_test_track_for_windows("Song");
        let mut updated = track.clone();
        updated.album = Some("Different Album".to_string());
        assert!(!track_identity_changed(Some(&track), &updated));
    }

    #[test]
    fn test_track_identity_changed_artist_changed() {
        let track = create_test_track_for_windows("Song");
        let mut updated = track.clone();
        updated.artist = vec!["Another Artist".to_string()];
        assert!(track_identity_changed(Some(&track), &updated));
    }

    // is_seek tests

    #[test]
    fn test_is_seek_first_position_always_true() {
        assert!(is_seek(None, Duration::from_secs(30)));
        assert!(is_seek(None, Duration::ZERO));
    }

    #[test]
    fn test_is_seek_normal_playback_not_reported() {
        // 1-second advance during normal playback must not trigger PositionChanged
        let last = Some(Duration::from_secs(10));
        assert!(!is_seek(last, Duration::from_secs(11)));
    }

    #[test]
    fn test_is_seek_at_threshold_not_reported() {
        // Exactly 2 seconds: boundary is exclusive (> 2s), so this is NOT a seek
        let last = Some(Duration::from_secs(10));
        assert!(!is_seek(last, Duration::from_secs(12)));
    }

    #[test]
    fn test_is_seek_exceeds_threshold() {
        let last = Some(Duration::from_secs(10));
        assert!(is_seek(last, Duration::from_secs(13)));
    }

    #[test]
    fn test_is_seek_backward_jump() {
        let last = Some(Duration::from_mins(1));
        assert!(is_seek(last, Duration::from_secs(10)));
    }

    #[test]
    fn test_is_seek_same_position_not_reported() {
        let last = Some(Duration::from_secs(30));
        assert!(!is_seek(last, Duration::from_secs(30)));
    }

    // diff_player_state tests

    #[test]
    fn test_diff_player_state_no_change_emits_nothing() {
        let state = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        // Same position so is_seek is false; nothing should be emitted.
        let events = diff_player_state("p", Some(&state), &state);
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_diff_player_state_track_change_emits_track_and_position() {
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Old"),
            PlaybackState::Playing,
            Some(Duration::from_mins(2)),
        );
        let new = create_test_state_for_windows(
            create_test_track_for_windows("New"),
            PlaybackState::Playing,
            Some(Duration::from_secs(0)),
        );
        let events = diff_player_state("p", Some(&old), &new);
        // Track changed, state unchanged, and the new track's position is reported as fresh
        // (track change resets the seek baseline).
        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "p".to_string(),
                    track: new.track,
                },
                MediaEvent::PositionChanged {
                    player_name: "p".to_string(),
                    position: Duration::from_secs(0),
                },
            ]
        );
    }

    #[test]
    fn test_diff_player_state_metadata_only_change_not_emitted() {
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let mut new = old.clone();
        // Late-loading metadata only: album/art change, identity (title+artist) stays.
        new.track.album = Some("Different Album".to_string());
        new.track.artwork = Some(Artwork::Url {
            url: "http://example.com/a.jpg".to_string(),
        });
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_diff_player_state_playback_state_change_only() {
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let mut new = old.clone();
        new.playback_state = PlaybackState::Paused;
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(
            events,
            vec![MediaEvent::StateChanged {
                player_name: "p".to_string(),
                state: PlaybackState::Paused,
            }]
        );
    }

    #[test]
    fn test_diff_player_state_seek_within_same_track() {
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let mut new = old.clone();
        new.position = Some(Duration::from_mins(1));
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "p".to_string(),
                position: Duration::from_mins(1),
            }]
        );
    }

    #[test]
    fn test_diff_player_state_normal_progression_not_emitted() {
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let mut new = old.clone();
        // 1-second advance during normal playback is not a seek.
        new.position = Some(Duration::from_secs(11));
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_diff_player_state_volume_change_only() {
        let mut old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        old.volume = Some(0.8);
        let mut new = old.clone();
        new.volume = Some(0.5);
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(
            events,
            vec![MediaEvent::VolumeChanged {
                player_name: "p".to_string(),
                volume: 0.5,
            }]
        );
    }

    #[test]
    fn test_diff_player_state_volume_absent_not_emitted() {
        // Both volumes None (e.g. the Windows backend): never emits VolumeChanged.
        let old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let new = old.clone();
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_diff_player_state_no_previous_emits_track_state_and_position() {
        // old = None (first discovery): TrackChanged + StateChanged + PositionChanged emitted,
        // but VolumeChanged is NOT emitted because the caller is expected to handle the baseline.
        let state = PlayerState {
            track: create_test_track_for_windows("New Song"),
            playback_state: PlaybackState::Playing,
            position: Some(Duration::from_secs(5)),
            volume: Some(0.8),
        };

        let events = diff_player_state("p", None, &state);

        assert_eq!(
            events,
            vec![
                MediaEvent::TrackChanged {
                    player_name: "p".to_string(),
                    track: state.track,
                },
                MediaEvent::StateChanged {
                    player_name: "p".to_string(),
                    state: PlaybackState::Playing,
                },
                MediaEvent::PositionChanged {
                    player_name: "p".to_string(),
                    position: Duration::from_secs(5),
                },
            ]
        );
    }

    #[test]
    fn test_diff_player_state_position_none_emits_no_position_changed() {
        // When new.position is None the PositionChanged branch is entirely skipped.
        let old = PlayerState {
            track: create_test_track_for_windows("Song"),
            playback_state: PlaybackState::Playing,
            position: Some(Duration::from_secs(10)),
            volume: None,
        };
        let new = PlayerState {
            position: None,
            ..old.clone()
        };

        let events = diff_player_state("p", Some(&old), &new);

        assert_eq!(events, Vec::<MediaEvent>::new());
    }

    #[test]
    fn test_diff_player_state_volume_becomes_none_not_emitted() {
        // old.volume = Some, new.volume = None: the third condition of the let-chain fails,
        // so VolumeChanged is not emitted.
        let mut old = create_test_state_for_windows(
            create_test_track_for_windows("Song"),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        old.volume = Some(0.8);
        let mut new = old.clone();
        new.volume = None;

        let events = diff_player_state("p", Some(&old), &new);

        assert_eq!(events, Vec::<MediaEvent>::new());
    }
}
