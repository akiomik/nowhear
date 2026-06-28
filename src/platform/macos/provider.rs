//! JXA-based player state provider for macOS.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{MediaSourceError, Result};
use crate::platform::state::PlayerState;
use crate::types::{PlaybackState, Track};

/// Internal trait for abstracting player state retrieval.
///
/// This trait is used internally by the macOS implementation to allow
/// for dependency injection in tests. It is not part of the public API.
pub trait PlayerStateProvider: Send + Sync {
    fn get_player_state(
        &self,
        player_name: &str,
    ) -> impl Future<Output = Result<Option<PlayerState>>> + Send;
    fn list_available_players(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    /// Retrieves the state of every supported player in a single query.
    ///
    /// The OSA backend fetches both Music.app and Spotify with a single script
    /// execution, halving the number of queries per poll compared to querying
    /// each player separately.
    fn get_all_player_states(&self) -> impl Future<Output = Result<AllPlayerStates>> + Send;
}

/// Snapshot of every supported macOS player, retrieved in a single query.
///
/// A `None` field means that player is not running (or not currently playing).
/// Not part of the public API.
pub struct AllPlayerStates {
    pub music: Option<PlayerState>,
    pub spotify: Option<PlayerState>,
}

/// JXA-based provider for macOS.
///
/// This provider uses JXA (JavaScript for Automation) via OSAKit to query Music.app and Spotify for their
/// current playback state.
pub struct AppleScriptProvider;

impl PlayerStateProvider for AppleScriptProvider {
    async fn get_player_state(&self, player_name: &str) -> Result<Option<PlayerState>> {
        // Validate the name before spawning osascript so unknown players fail cheaply.
        if !matches!(player_name, "Music" | "Spotify") {
            return Err(MediaSourceError::PlayerNotFound(player_name.to_string()));
        }

        let states = self.get_all_player_states().await?;
        Ok(match player_name {
            "Spotify" => states.spotify,
            _ => states.music,
        })
    }

    async fn list_available_players(&self) -> Result<Vec<String>> {
        // A transient failure is treated as "no players running" to match the
        // previous per-player `.ok().flatten()` behavior.
        let states = self
            .get_all_player_states()
            .await
            .unwrap_or(AllPlayerStates {
                music: None,
                spotify: None,
            });

        let mut players = Vec::new();
        if states.music.is_some() {
            players.push("Music".to_string());
        }
        if states.spotify.is_some() {
            players.push("Spotify".to_string());
        }

        Ok(players)
    }

    async fn get_all_player_states(&self) -> Result<AllPlayerStates> {
        // Determine which players are running in-process; this is far cheaper
        // than an Apple Event `running` check and lets us skip OSA entirely when
        // nothing is playing.
        let Some((cache_key, source)) = build_script(super::running::running_players()) else {
            return Ok(AllPlayerStates {
                music: None,
                spotify: None,
            });
        };

        let output = super::osa::execute(cache_key, source).await?;
        let raw: AllAppleScriptStates = serde_json::from_str(&output)?;

        Ok(AllPlayerStates {
            music: raw
                .music
                .map(AppleScriptPlayerState::into_music_player_state),
            spotify: raw
                .spotify
                .map(AppleScriptPlayerState::into_spotify_player_state),
        })
    }
}

/// Builds the OSA script for the currently running players, or `None` when no
/// supported player is running (in which case the query is skipped entirely).
///
/// The JXA file defines only helpers; the entry point composed here invokes
/// `safeGetState` solely for running players, so the script never needs its own
/// (expensive) `app.running()` check. The returned cache key is stable per
/// running-set so each of the three variants is compiled at most once.
fn build_script(running: super::running::RunningPlayers) -> Option<(&'static str, String)> {
    const HELPERS: &str = include_str!("jxa/player_states.js");
    const MUSIC: &str = r#"safeGetState("Music", readTrackProperties, false)"#;
    const SPOTIFY: &str = r#"safeGetState("Spotify", readTrackIndividually, true)"#;

    let (cache_key, music_expr, spotify_expr) = match (running.music, running.spotify) {
        (false, false) => return None,
        (true, true) => ("both", MUSIC, SPOTIFY),
        (true, false) => ("music", MUSIC, "null"),
        (false, true) => ("spotify", "null", SPOTIFY),
    };

    let source =
        format!("{HELPERS}\nJSON.stringify({{ music: {music_expr}, spotify: {spotify_expr} }});");
    Some((cache_key, source))
}

/// Deserialization target for the `jxa/player_states.js` output.
///
/// Each field is `None` when the corresponding player is not running or not
/// currently playing.
#[derive(Debug, Deserialize)]
struct AllAppleScriptStates {
    music: Option<AppleScriptPlayerState>,
    spotify: Option<AppleScriptPlayerState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum AppleScriptPlaybackState {
    #[serde(rename = "stopped")]
    Stopped,
    #[serde(rename = "playing")]
    Playing,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "fast forwarding")]
    FastForwarding,
    #[serde(rename = "rewinding")]
    Rewinding,
}

/// Intermediate representation of player state from JXA.
///
/// This struct is used to deserialize the JSON output of the JXA script that
/// queries Music.app and Spotify. It serves as a bridge between the JXA
/// layer and the internal [`PlayerState`] representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleScriptPlayerState {
    player_state: AppleScriptPlaybackState,
    player_position: f64,
    sound_volume: usize,
    track_name: String,
    track_artist: String,
    track_album: String,
    track_album_artist: String,
    track_album_artwork_url: Option<String>,
    track_number: u32,
    track_duration: f64,
}

impl AppleScriptPlayerState {
    pub const fn playback_state(&self) -> PlaybackState {
        match self.player_state {
            AppleScriptPlaybackState::Paused => PlaybackState::Paused,
            AppleScriptPlaybackState::Stopped => PlaybackState::Stopped,
            _ => PlaybackState::Playing,
        }
    }

    pub fn into_music_track(self) -> Track {
        Track {
            title: self.track_name,
            artist: if self.track_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_artist]
            },
            album: if self.track_album.is_empty() {
                None
            } else {
                Some(self.track_album)
            },
            album_artist: if self.track_album_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_album_artist]
            },
            track_number: Some(self.track_number),
            duration: Some(Duration::from_secs_f64(self.track_duration)),
            art_url: self.track_album_artwork_url.filter(|s| !s.is_empty()),
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn into_spotify_track(self) -> Track {
        Track {
            title: self.track_name,
            artist: if self.track_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_artist]
            },
            album: if self.track_album.is_empty() {
                None
            } else {
                Some(self.track_album)
            },
            album_artist: if self.track_album_artist.is_empty() {
                vec![]
            } else {
                vec![self.track_album_artist]
            },
            track_number: Some(self.track_number),
            duration: Some(Duration::from_millis(self.track_duration as u64)),
            art_url: self.track_album_artwork_url.filter(|s| !s.is_empty()),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn into_music_player_state(self) -> PlayerState {
        let player_position = Duration::from_secs_f64(self.player_position);
        let sound_volume = self.sound_volume as f64 / 100.0;
        let playback_state = self.playback_state();

        PlayerState {
            track: self.into_music_track(),
            playback_state,
            position: Some(player_position),
            volume: Some(sound_volume),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn into_spotify_player_state(self) -> PlayerState {
        let player_position = Duration::from_secs_f64(self.player_position);
        let sound_volume = self.sound_volume as f64 / 100.0;
        let playback_state = self.playback_state();

        PlayerState {
            track: self.into_spotify_track(),
            playback_state,
            position: Some(player_position),
            volume: Some(sound_volume),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::running;
    use super::*;

    // build_script tests

    #[test]
    fn test_build_script_skips_when_nothing_running() {
        let result = build_script(running::RunningPlayers {
            music: false,
            spotify: false,
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_build_script_queries_only_running_players() {
        for (running, key, want_music, want_spotify) in [
            (
                running::RunningPlayers {
                    music: true,
                    spotify: true,
                },
                "both",
                true,
                true,
            ),
            (
                running::RunningPlayers {
                    music: true,
                    spotify: false,
                },
                "music",
                true,
                false,
            ),
            (
                running::RunningPlayers {
                    music: false,
                    spotify: true,
                },
                "spotify",
                false,
                true,
            ),
        ] {
            let (cache_key, source) = build_script(running).expect("a player is running");
            assert_eq!(cache_key, key);

            // A non-running player must not be queried (querying would launch it).
            assert_eq!(source.contains(r#"safeGetState("Music""#), want_music);
            assert_eq!(source.contains(r#"safeGetState("Spotify""#), want_spotify);
            // The composed entry point must be present.
            assert!(source.contains("JSON.stringify({ music:"));
        }
    }

    #[test]
    fn test_all_applescript_states_deserialize() -> Result<()> {
        let json = r#"{
            "music": {
                "playerState": "playing",
                "playerPosition": 45.5,
                "soundVolume": 80,
                "trackName": "Music Song",
                "trackArtist": "Music Artist",
                "trackAlbum": "Music Album",
                "trackAlbumArtist": "Music Album Artist",
                "trackNumber": 3,
                "trackDuration": 180.0
            },
            "spotify": null
        }"#;

        let states: AllAppleScriptStates = serde_json::from_str(json)?;

        assert_eq!(
            states.music.map(|s| s.track_name),
            Some("Music Song".to_string())
        );
        assert_eq!(states.spotify, None);

        Ok(())
    }

    #[test]
    fn test_all_applescript_states_deserialize_both_null() -> Result<()> {
        let json = r#"{ "music": null, "spotify": null }"#;

        let states: AllAppleScriptStates = serde_json::from_str(json)?;

        assert_eq!(states.music, None);
        assert_eq!(states.spotify, None);

        Ok(())
    }

    // AppleScriptPlayerState tests

    #[test]
    fn test_applescript_player_state_deserialize_playing() -> Result<()> {
        let json = r#"{
            "playerState": "playing",
            "playerPosition": 45.5,
            "soundVolume": 80,
            "trackName": "Test Song",
            "trackArtist": "Test Artist",
            "trackAlbum": "Test Album",
            "trackAlbumArtist": "Test Album Artist",
            "trackNumber": 3,
            "trackDuration": 180.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(
            state,
            AppleScriptPlayerState {
                player_state: AppleScriptPlaybackState::Playing,
                player_position: 45.5,
                sound_volume: 80,
                track_name: "Test Song".to_string(),
                track_artist: "Test Artist".to_string(),
                track_album: "Test Album".to_string(),
                track_album_artist: "Test Album Artist".to_string(),
                track_album_artwork_url: None,
                track_number: 3,
                track_duration: 180.0,
            }
        );

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_paused() -> Result<()> {
        let json = r#"{
            "playerState": "paused",
            "playerPosition": 30.0,
            "soundVolume": 50,
            "trackName": "Paused Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 1,
            "trackDuration": 200.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Paused);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_stopped() -> Result<()> {
        let json = r#"{
            "playerState": "stopped",
            "playerPosition": 0.0,
            "soundVolume": 70,
            "trackName": "Stopped Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 2,
            "trackDuration": 150.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Stopped);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_fast_forwarding() -> Result<()> {
        let json = r#"{
            "playerState": "fast forwarding",
            "playerPosition": 60.0,
            "soundVolume": 90,
            "trackName": "Fast Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 5,
            "trackDuration": 240.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::FastForwarding);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_rewinding() -> Result<()> {
        let json = r#"{
            "playerState": "rewinding",
            "playerPosition": 20.0,
            "soundVolume": 60,
            "trackName": "Rewind Song",
            "trackArtist": "Artist",
            "trackAlbum": "Album",
            "trackAlbumArtist": "Album Artist",
            "trackNumber": 4,
            "trackDuration": 210.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(state.player_state, AppleScriptPlaybackState::Rewinding);

        Ok(())
    }

    #[test]
    fn test_applescript_player_state_deserialize_track_album_artwork_url() -> Result<()> {
        let json = r#"{
            "playerState": "playing",
            "playerPosition": 45.5,
            "soundVolume": 80,
            "trackName": "Test Song",
            "trackArtist": "Test Artist",
            "trackAlbum": "Test Album",
            "trackAlbumArtist": "Test Album Artist",
            "trackAlbumArtworkUrl": "https://example.com/image/deadbeef",
            "trackNumber": 3,
            "trackDuration": 180.0
        }"#;

        let state: AppleScriptPlayerState = serde_json::from_str(json)?;

        assert_eq!(
            state,
            AppleScriptPlayerState {
                player_state: AppleScriptPlaybackState::Playing,
                player_position: 45.5,
                sound_volume: 80,
                track_name: "Test Song".to_string(),
                track_artist: "Test Artist".to_string(),
                track_album: "Test Album".to_string(),
                track_album_artist: "Test Album Artist".to_string(),
                track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
                track_number: 3,
                track_duration: 180.0,
            }
        );

        Ok(())
    }

    #[test]
    fn test_applescript_playback_state_conversion() {
        let test_cases = vec![
            (AppleScriptPlaybackState::Playing, PlaybackState::Playing),
            (AppleScriptPlaybackState::Paused, PlaybackState::Paused),
            (AppleScriptPlaybackState::Stopped, PlaybackState::Stopped),
            (
                AppleScriptPlaybackState::FastForwarding,
                PlaybackState::Playing,
            ),
            (AppleScriptPlaybackState::Rewinding, PlaybackState::Playing),
        ];

        for (applescript_state, expected_state) in test_cases {
            let state = AppleScriptPlayerState {
                player_state: applescript_state,
                player_position: 0.0,
                sound_volume: 50,
                track_name: "Test".to_string(),
                track_artist: "Artist".to_string(),
                track_album: "Album".to_string(),
                track_album_artist: "Album Artist".to_string(),
                track_album_artwork_url: None,
                track_number: 1,
                track_duration: 100.0,
            };

            assert_eq!(state.playback_state(), expected_state);
        }
    }

    #[test]
    fn test_applescript_player_state_into_music_track() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 45.5,
            sound_volume: 80,
            track_name: "Bohemian Rhapsody".to_string(),
            track_artist: "Queen".to_string(),
            track_album: "A Night at the Opera".to_string(),
            track_album_artist: "Queen".to_string(),
            track_album_artwork_url: None,
            track_number: 11,
            track_duration: 354.0, // seconds
        };

        let track = state.into_music_track();

        assert_eq!(
            track,
            Track {
                title: "Bohemian Rhapsody".to_string(),
                artist: vec!["Queen".to_string()],
                album: Some("A Night at the Opera".to_string()),
                album_artist: vec!["Queen".to_string()],
                track_number: Some(11),
                duration: Some(Duration::from_secs_f64(354.0)),
                art_url: None,
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_spotify_track() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 30.0,
            sound_volume: 70,
            track_name: "Stairway to Heaven".to_string(),
            track_artist: "Led Zeppelin".to_string(),
            track_album: "Led Zeppelin IV".to_string(),
            track_album_artist: "Led Zeppelin".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 4,
            track_duration: 482_000.0, // milliseconds
        };

        let track = state.into_spotify_track();

        assert_eq!(
            track,
            Track {
                title: "Stairway to Heaven".to_string(),
                artist: vec!["Led Zeppelin".to_string()],
                album: Some("Led Zeppelin IV".to_string()),
                album_artist: vec!["Led Zeppelin".to_string()],
                track_number: Some(4),
                duration: Some(Duration::from_secs(482)),
                art_url: Some("https://example.com/image/deadbeef".to_owned()),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_music_player_state() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Paused,
            player_position: 120.5,
            sound_volume: 75,
            track_name: "Test Track".to_string(),
            track_artist: "Test Artist".to_string(),
            track_album: "Test Album".to_string(),
            track_album_artist: "Test Album Artist".to_string(),
            track_album_artwork_url: None,
            track_number: 2,
            track_duration: 240.0,
        };

        let player_state = state.into_music_player_state();

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Test Track".to_string(),
                    artist: vec!["Test Artist".to_string()],
                    album: Some("Test Album".to_string()),
                    album_artist: vec!["Test Album Artist".to_string()],
                    track_number: Some(2),
                    duration: Some(Duration::from_secs_f64(240.0)),
                    art_url: None,
                },
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs_f64(120.5)),
                volume: Some(0.75),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_into_spotify_player_state() {
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 65.25,
            sound_volume: 85,
            track_name: "Spotify Track".to_string(),
            track_artist: "Spotify Artist".to_string(),
            track_album: "Spotify Album".to_string(),
            track_album_artist: "Spotify Album Artist".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 7,
            track_duration: 195_000.0, // milliseconds for Spotify
        };

        let player_state = state.into_spotify_player_state();

        assert_eq!(
            player_state,
            PlayerState {
                track: Track {
                    title: "Spotify Track".to_string(),
                    artist: vec!["Spotify Artist".to_string()],
                    album: Some("Spotify Album".to_string()),
                    album_artist: vec!["Spotify Album Artist".to_string()],
                    track_number: Some(7),
                    duration: Some(Duration::from_secs(195)),
                    art_url: Some("https://example.com/image/deadbeef".to_owned()),
                },
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs_f64(65.25)),
                volume: Some(0.85),
            }
        );
    }

    #[test]
    fn test_applescript_player_state_duration_difference() {
        // Music.app uses seconds for duration
        let music_state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: "Artist".to_string(),
            track_album: "Album".to_string(),
            track_album_artist: "Album Artist".to_string(),
            track_album_artwork_url: None,
            track_number: 1,
            track_duration: 180.0, // 180 seconds
        };

        // Spotify uses milliseconds for duration
        let spotify_state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: "Artist".to_string(),
            track_album: "Album".to_string(),
            track_album_artist: "Album Artist".to_string(),
            track_album_artwork_url: Some("https://example.com/image/deadbeef".to_owned()),
            track_number: 1,
            track_duration: 180_000.0, // 180000 milliseconds = 180 seconds
        };

        let music_track = music_state.into_music_track();
        let spotify_track = spotify_state.into_spotify_track();

        // Both should represent the same duration
        assert_eq!(music_track.duration, Some(Duration::from_mins(3)));
        assert_eq!(spotify_track.duration, Some(Duration::from_mins(3)));
        assert_eq!(music_track.duration, spotify_track.duration);
    }

    // Empty-string field handling in into_music_track / into_spotify_track

    #[test]
    fn test_applescript_player_state_into_music_track_empty_fields() {
        // Empty strings for artist, album, album_artist, and art_url must map to the
        // "absent" representation (vec![] / None) rather than leaking empty strings.
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: String::new(),
            track_album: String::new(),
            track_album_artist: String::new(),
            track_album_artwork_url: Some(String::new()),
            track_number: 1,
            track_duration: 180.0,
        };

        let track = state.into_music_track();

        assert_eq!(track.artist, Vec::<String>::new());
        assert_eq!(track.album, None);
        assert_eq!(track.album_artist, Vec::<String>::new());
        assert_eq!(track.art_url, None);
    }

    #[test]
    fn test_applescript_player_state_into_spotify_track_empty_fields() {
        // Same empty-field contract for the Spotify variant.
        let state = AppleScriptPlayerState {
            player_state: AppleScriptPlaybackState::Playing,
            player_position: 0.0,
            sound_volume: 50,
            track_name: "Track".to_string(),
            track_artist: String::new(),
            track_album: String::new(),
            track_album_artist: String::new(),
            track_album_artwork_url: Some(String::new()),
            track_number: 1,
            track_duration: 180_000.0,
        };

        let track = state.into_spotify_track();

        assert_eq!(track.artist, Vec::<String>::new());
        assert_eq!(track.album, None);
        assert_eq!(track.album_artist, Vec::<String>::new());
        assert_eq!(track.art_url, None);
    }

    // AppleScriptProvider::get_player_state validates the player name before any OSA call

    #[tokio::test]
    async fn test_apple_script_provider_get_player_state_unknown_player() {
        use crate::MediaSourceError;

        let provider = AppleScriptProvider;
        let result = provider.get_player_state("UnknownPlayer").await;
        assert_eq!(
            result,
            Err(MediaSourceError::PlayerNotFound(
                "UnknownPlayer".to_string()
            ))
        );
    }
}
