//! In-process detection of which media players are currently running.
//!
//! The JXA scripts used to gate each query on `app.running()`, but profiling
//! showed that check is surprisingly expensive: every call resolves the app by
//! path through `LaunchServices`, accounting for a large share of the remaining
//! per-poll CPU while playing.
//!
//! `NSRunningApplication` answers the same question from an in-process,
//! `LaunchServices`-cached list — roughly an order of magnitude cheaper — and
//! lets the poller skip OSA execution entirely when nothing is running.

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

/// Bundle identifier of Music.app (formerly iTunes).
const MUSIC_BUNDLE_ID: &str = "com.apple.Music";
/// Bundle identifier of the Spotify desktop client.
const SPOTIFY_BUNDLE_ID: &str = "com.spotify.client";

/// Which supported players currently have a running process.
///
/// A `false` field means the app is not running, so it must not be queried
/// (touching a non-running app via Apple Events would launch it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunningPlayers {
    pub music: bool,
    pub spotify: bool,
}

/// Checks which supported players are running, in-process.
pub fn running_players() -> RunningPlayers {
    RunningPlayers {
        music: is_running(MUSIC_BUNDLE_ID),
        spotify: is_running(SPOTIFY_BUNDLE_ID),
    }
}

/// Returns `true` when at least one running process has `bundle_id`.
fn is_running(bundle_id: &str) -> bool {
    autoreleasepool(|_| {
        let id = NSString::from_str(bundle_id);
        !NSRunningApplication::runningApplicationsWithBundleIdentifier(&id).is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_players_does_not_panic() {
        // Exercises the real NSRunningApplication call; the result depends on
        // the environment, so we only assert it returns without panicking.
        let _ = running_players();
    }
}
