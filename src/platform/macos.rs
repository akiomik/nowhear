//! macOS-specific implementation of the media watcher.
//!
//! This module provides media monitoring functionality for macOS using AppleScript
//! to interact with media players like Music.app and Spotify.

#[cfg(target_os = "macos")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, PlayerState, Track};
use crate::watcher::{EventStream, MediaWatcher};
use async_trait::async_trait;
use futures::stream::Stream;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Media watcher implementation for macOS.
///
/// This watcher monitors media players on macOS by executing AppleScript commands
/// to query the current playback state. It supports Music.app and Spotify.
pub struct MacOSMediaWatcher;

impl MacOSMediaWatcher {
    /// Creates a new macOS media watcher.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing the newly created watcher instance.
    pub async fn new() -> Result<Self> {
        Ok(Self {})
    }

    /// Retrieves the current playback state from Music.app using AppleScript.
    ///
    /// This method executes an AppleScript to query Music.app for its current playback state.
    /// The application will not be launched if it's not already running.
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(PlayerState))` if Music.app is running and playing/paused,
    /// `Ok(None)` if the app is not running or not playing, or an error if the query fails.
    async fn get_music_app_state(&self) -> Result<Option<PlayerState>> {
        let script = r#"
            if application "Music" is running then
                tell application "Music"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }

    /// Retrieves the current playback state from Spotify using AppleScript.
    ///
    /// This method executes an AppleScript to query Spotify for its current playback state.
    /// The application will not be launched if it's not already running.
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(PlayerState))` if Spotify is running and playing/paused,
    /// `Ok(None)` if the app is not running or not playing, or an error if the query fails.
    async fn get_spotify_state(&self) -> Result<Option<PlayerState>> {
        let script = r#"
            if application "Spotify" is running then
                tell application "Spotify"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }

    /// Creates an event stream using AppleScript polling.
    ///
    /// This method creates a stream that emits `MediaEvent` items whenever media playback
    /// state changes. The implementation uses periodic polling (every 1 second) to check
    /// the state of Music.app and Spotify.
    ///
    /// # Implementation Note
    ///
    /// An implementation using `NSDistributedNotificationCenter` was considered but not adopted
    /// because it requires execution on the main thread. This polling-based approach using
    /// AppleScript provides a simpler alternative that works in async contexts.
    ///
    /// # Returns
    ///
    /// Returns a stream that yields `MediaEvent` items when track changes are detected.
    fn create_event_stream_impl() -> impl Stream<Item = MediaEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut last_music_track: Option<Track> = None;
            let mut last_music_state: Option<PlaybackState> = None;
            let mut last_music_position: Option<Duration> = None;
            let mut last_music_volume: Option<f64> = None;
            let mut last_spotify_track: Option<Track> = None;
            let mut last_spotify_state: Option<PlaybackState> = None;
            let mut last_spotify_position: Option<Duration> = None;
            let mut last_spotify_volume: Option<f64> = None;
            let mut music_app_running = false;
            let mut spotify_running = false;
            let mut interval = time::interval(Duration::from_millis(1000));

            loop {
                interval.tick().await;

                // Check Music.app state
                if let Ok(Some(player_state)) = Self::poll_music_app().await {
                    // Check if Music.app just started
                    if !music_app_running {
                        let event = MediaEvent::PlayerAdded {
                            player_name: "Music".to_string(),
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        music_app_running = true;
                    }

                    // Check for track change
                    if last_music_track.as_ref() != Some(&player_state.track) {
                        let event = MediaEvent::TrackChanged {
                            player_name: "Music".to_string(),
                            track: player_state.track.clone(),
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        last_music_track = Some(player_state.track.clone());
                    }

                    // Check for playback state change
                    if last_music_state != Some(player_state.playback_state) {
                        let event = MediaEvent::StateChanged {
                            player_name: "Music".to_string(),
                            state: player_state.playback_state,
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        last_music_state = Some(player_state.playback_state);
                    }

                    // Check for position change (seek detection - significant jump)
                    if let Some(pos) = player_state.position {
                        let should_emit = if let Some(last_pos) = last_music_position {
                            // Detect seek: position difference > 2 seconds
                            let diff = pos.abs_diff(last_pos);
                            diff > Duration::from_secs(2)
                        } else {
                            true // First position
                        };

                        if should_emit {
                            let event = MediaEvent::PositionChanged {
                                player_name: "Music".to_string(),
                                position: pos,
                            };
                            if tx.send(event).is_err() {
                                break; // Receiver dropped
                            }
                        }
                        last_music_position = Some(pos);
                    }

                    // Check for volume change
                    if player_state.volume != last_music_volume {
                        if let Some(vol) = player_state.volume {
                            let event = MediaEvent::VolumeChanged {
                                player_name: "Music".to_string(),
                                volume: vol,
                            };
                            if tx.send(event).is_err() {
                                break; // Receiver dropped
                            }
                        }
                        last_music_volume = player_state.volume;
                    }
                } else if music_app_running {
                    // Music.app has stopped
                    let event = MediaEvent::PlayerRemoved {
                        player_name: "Music".to_string(),
                    };
                    if tx.send(event).is_err() {
                        break; // Receiver dropped
                    }
                    music_app_running = false;
                    last_music_track = None;
                    last_music_state = None;
                    last_music_position = None;
                    last_music_volume = None;
                }

                // Check Spotify state
                if let Ok(Some(player_state)) = Self::poll_spotify().await {
                    // Check if Spotify just started
                    if !spotify_running {
                        let event = MediaEvent::PlayerAdded {
                            player_name: "Spotify".to_string(),
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        spotify_running = true;
                    }

                    // Check for track change
                    if last_spotify_track.as_ref() != Some(&player_state.track) {
                        let event = MediaEvent::TrackChanged {
                            player_name: "Spotify".to_string(),
                            track: player_state.track.clone(),
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        last_spotify_track = Some(player_state.track.clone());
                    }

                    // Check for playback state change
                    if last_spotify_state != Some(player_state.playback_state) {
                        let event = MediaEvent::StateChanged {
                            player_name: "Spotify".to_string(),
                            state: player_state.playback_state,
                        };
                        if tx.send(event).is_err() {
                            break; // Receiver dropped
                        }
                        last_spotify_state = Some(player_state.playback_state);
                    }

                    // Check for position change (seek detection - significant jump)
                    if let Some(pos) = player_state.position {
                        let should_emit = if let Some(last_pos) = last_spotify_position {
                            // Detect seek: position difference > 2 seconds
                            let diff = pos.abs_diff(last_pos);
                            diff > Duration::from_secs(2)
                        } else {
                            true // First position
                        };

                        if should_emit {
                            let event = MediaEvent::PositionChanged {
                                player_name: "Spotify".to_string(),
                                position: pos,
                            };
                            if tx.send(event).is_err() {
                                break; // Receiver dropped
                            }
                        }
                        last_spotify_position = Some(pos);
                    }

                    // Check for volume change
                    if player_state.volume != last_spotify_volume {
                        if let Some(vol) = player_state.volume {
                            let event = MediaEvent::VolumeChanged {
                                player_name: "Spotify".to_string(),
                                volume: vol,
                            };
                            if tx.send(event).is_err() {
                                break; // Receiver dropped
                            }
                        }
                        last_spotify_volume = player_state.volume;
                    }
                } else if spotify_running {
                    // Spotify has stopped
                    let event = MediaEvent::PlayerRemoved {
                        player_name: "Spotify".to_string(),
                    };
                    if tx.send(event).is_err() {
                        break; // Receiver dropped
                    }
                    spotify_running = false;
                    last_spotify_track = None;
                    last_spotify_state = None;
                    last_spotify_position = None;
                    last_spotify_volume = None;
                }
            }
        });

        UnboundedReceiverStream::new(rx)
    }

    /// Polls the current state of Music.app.
    ///
    /// This method queries Music.app for its current playback state without launching
    /// the application if it's not already running.
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(PlayerState))` if Music.app is running and playing/paused,
    /// `Ok(None)` if the app is not running or not in a playback state.
    async fn poll_music_app() -> Result<Option<PlayerState>> {
        let script = r#"
            if application "Music" is running then
                tell application "Music"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }

    /// Polls the current state of Spotify.
    ///
    /// This method queries Spotify for its current playback state without launching
    /// the application if it's not already running.
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(PlayerState))` if Spotify is running and playing/paused,
    /// `Ok(None)` if the app is not running or not in a playback state.
    async fn poll_spotify() -> Result<Option<PlayerState>> {
        let script = r#"
            if application "Spotify" is running then
                tell application "Spotify"
                    if player state is playing or player state is paused then
                        set trackName to name of current track
                        set trackArtist to artist of current track
                        set trackAlbum to album of current track
                        set playerState to player state as string
                        set playerPos to player position as string
                        set soundVol to sound volume as string
                        return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol
                    end if
                end tell
            end if
            return ""
        "#;

        let output = execute_applescript(script).await?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(parse_apple_script_output(&output))
        }
    }
}

#[async_trait]
impl MediaWatcher for MacOSMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        let mut players = Vec::new();

        if self.get_music_app_state().await.ok().flatten().is_some() {
            players.push("Music".to_string());
        }

        if self.get_spotify_state().await.ok().flatten().is_some() {
            players.push("Spotify".to_string());
        }

        Ok(players)
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        match player_name {
            "Music" => {
                if let Some(player_state) = self.get_music_app_state().await? {
                    Ok(PlayerInfo {
                        player_name: "Music".to_string(),
                        current_track: Some(player_state.track),
                        playback_state: player_state.playback_state,
                        position: player_state.position,
                        volume: player_state.volume,
                    })
                } else {
                    Ok(PlayerInfo::empty("Music"))
                }
            }
            "Spotify" => {
                if let Some(player_state) = self.get_spotify_state().await? {
                    Ok(PlayerInfo {
                        player_name: "Spotify".to_string(),
                        current_track: Some(player_state.track),
                        playback_state: player_state.playback_state,
                        position: player_state.position,
                        volume: player_state.volume,
                    })
                } else {
                    Ok(PlayerInfo::empty("Spotify"))
                }
            }
            _ => Err(MediaWatcherError::PlayerNotFound(player_name.to_string())),
        }
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = Self::create_event_stream_impl();
        Ok(Box::pin(stream))
    }
}

/// Executes an AppleScript and returns its output.
///
/// This function spawns the `osascript` command asynchronously to execute the provided
/// AppleScript code.
///
/// # Arguments
///
/// * `script` - The AppleScript code to execute
///
/// # Returns
///
/// Returns the trimmed stdout output of the script, or an error if execution fails.
async fn execute_applescript(script: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await
        .map_err(|e| {
            MediaWatcherError::InternalError(format!("Failed to execute AppleScript: {}", e))
        })?;

    if !output.status.success() {
        return Err(MediaWatcherError::InternalError(format!(
            "AppleScript error: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parses the tab-separated output from AppleScript into a `PlayerState` structure.
///
/// The expected format is: `title\tartist\talbum\tplayerState\tposition\tvolume`
///
/// # Arguments
///
/// * `output` - The tab-separated string returned by AppleScript
///
/// # Returns
///
/// Returns `Some(PlayerState)` if the output can be parsed successfully, `None` otherwise.
fn parse_apple_script_output(output: &str) -> Option<PlayerState> {
    let parts: Vec<&str> = output.split('\t').collect();

    if parts.len() >= 3 {
        let track = Track {
            title: parts[0].to_string(),
            artist: vec![parts[1].to_string()],
            album: if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            },
            album_artist: None,
            track_number: None,
            duration: None,
            art_url: None,
        };

        // Parse the playback state from the 4th field if available
        let playback_state = if parts.len() >= 4 {
            match parts[3].trim().to_lowercase().as_str() {
                "playing" => PlaybackState::Playing,
                "paused" => PlaybackState::Paused,
                _ => PlaybackState::Playing, // Default to Playing for unknown states
            }
        } else {
            PlaybackState::Playing // Default if state is not provided
        };

        // Parse position (in seconds) from the 5th field if available
        let position = if parts.len() >= 5 {
            parts[4]
                .trim()
                .parse::<f64>()
                .ok()
                .map(Duration::from_secs_f64)
        } else {
            None
        };

        // Parse volume from the 6th field if available
        let volume = if parts.len() >= 6 {
            parts[5].trim().parse::<f64>().ok().map(|vol| vol / 100.0) // Convert 0-100 to 0.0-1.0
        } else {
            None
        };

        Some(PlayerState {
            track,
            playback_state,
            position,
            volume,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_applescript_output_with_full_info() {
        let output = "Bohemian Rhapsody\tQueen\tA Night at the Opera\tplaying\t123.45\t75";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Bohemian Rhapsody");
        assert_eq!(player_state.track.artist, vec!["Queen"]);
        assert_eq!(
            player_state.track.album,
            Some("A Night at the Opera".to_string())
        );
        assert_eq!(player_state.track.album_artist, None);
        assert_eq!(player_state.track.track_number, None);
        assert_eq!(player_state.track.duration, None);
        assert_eq!(player_state.track.art_url, None);
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
        assert_eq!(player_state.position, Some(Duration::from_secs_f64(123.45)));
        assert_eq!(player_state.volume, Some(0.75));
    }

    #[test]
    fn test_parse_applescript_output_with_empty_album() {
        let output = "Test Song\tTest Artist\t\tplaying\t0\t100";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Test Song");
        assert_eq!(player_state.track.artist, vec!["Test Artist"]);
        assert_eq!(player_state.track.album, None);
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
        assert_eq!(player_state.position, Some(Duration::from_secs(0)));
        assert_eq!(player_state.volume, Some(1.0));
    }

    #[test]
    fn test_parse_applescript_output_with_paused_state() {
        let output = "Some Track\tSome Artist\tSome Album\tpaused\t60.5\t50";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Some Track");
        assert_eq!(player_state.track.artist, vec!["Some Artist"]);
        assert_eq!(player_state.track.album, Some("Some Album".to_string()));
        assert_eq!(player_state.playback_state, PlaybackState::Paused);
        assert_eq!(player_state.position, Some(Duration::from_secs_f64(60.5)));
        assert_eq!(player_state.volume, Some(0.5));
    }

    #[test]
    fn test_parse_applescript_output_with_special_characters() {
        let output = "Song & Title (Remix)\tArtist: Name\tAlbum - Edition\tplaying\t30\t80";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Song & Title (Remix)");
        assert_eq!(player_state.track.artist, vec!["Artist: Name"]);
        assert_eq!(
            player_state.track.album,
            Some("Album - Edition".to_string())
        );
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn test_parse_applescript_output_with_unicode() {
        let output = "春よ、来い\t松任谷由実\tThe Dancing Sun\tplaying\t120\t65";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "春よ、来い");
        assert_eq!(player_state.track.artist, vec!["松任谷由実"]);
        assert_eq!(
            player_state.track.album,
            Some("The Dancing Sun".to_string())
        );
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn test_parse_applescript_output_with_insufficient_parts() {
        let output = "Only Title\tOnly Artist";
        let result = parse_apple_script_output(output);

        assert!(result.is_none());
    }

    #[test]
    fn test_parse_applescript_output_with_empty_string() {
        let output = "";
        let result = parse_apple_script_output(output);

        assert!(result.is_none());
    }

    #[test]
    fn test_parse_applescript_output_with_single_field() {
        let output = "Just a title";
        let result = parse_apple_script_output(output);

        assert!(result.is_none());
    }

    #[test]
    fn test_parse_applescript_output_without_position_volume() {
        // Test when only 4 fields are provided (no position and volume)
        let output = "Title\tArtist\tAlbum\tplaying";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Title");
        assert_eq!(player_state.track.artist, vec!["Artist"]);
        assert_eq!(player_state.track.album, Some("Album".to_string()));
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
        assert_eq!(player_state.position, None);
        assert_eq!(player_state.volume, None);
    }

    #[test]
    fn test_parse_applescript_output_with_whitespace() {
        let output = "  Trimmed Title  \t  Trimmed Artist  \t  Trimmed Album  \tplaying\t10\t90";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        // Note: The function doesn't trim individual fields, it preserves whitespace
        assert_eq!(player_state.track.title, "  Trimmed Title  ");
        assert_eq!(player_state.track.artist, vec!["  Trimmed Artist  "]);
        assert_eq!(
            player_state.track.album,
            Some("  Trimmed Album  ".to_string())
        );
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn test_parse_applescript_output_without_state() {
        // Test when only 3 fields are provided (no player state)
        let output = "Title\tArtist\tAlbum";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.track.title, "Title");
        assert_eq!(player_state.track.artist, vec!["Artist"]);
        assert_eq!(player_state.track.album, Some("Album".to_string()));
        // Should default to Playing when state is not provided
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
        assert_eq!(player_state.position, None);
        assert_eq!(player_state.volume, None);
    }

    #[test]
    fn test_parse_applescript_output_with_uppercase_state() {
        // Test case insensitivity
        let output = "Title\tArtist\tAlbum\tPLAYING\t45\t55";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn test_parse_applescript_output_with_unknown_state() {
        // Test unknown state defaults to Playing
        let output = "Title\tArtist\tAlbum\tstopped\t0\t0";
        let result = parse_apple_script_output(output);

        assert!(result.is_some());
        let player_state = result.unwrap();
        assert_eq!(player_state.playback_state, PlaybackState::Playing);
    }
}
