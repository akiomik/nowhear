#[cfg(target_os = "windows")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use async_trait::async_trait;
use futures::stream::Stream;

pub struct WindowsMediaWatcher {
    // TODO: Store GlobalSystemMediaTransportControlsSessionManager handle
}

impl WindowsMediaWatcher {
    pub async fn new() -> Result<Self> {
        // TODO: Initialize Windows Runtime
        // Use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager

        Ok(Self {})
    }

    async fn get_current_session(&self) -> Result<Option<MediaSession>> {
        // TODO: Get current media session from
        // GlobalSystemMediaTransportControlsSessionManager::RequestAsync()

        Ok(None)
    }

    fn create_event_stream_impl(&self) -> impl Stream<Item = MediaEvent> {
        // TODO: Subscribe to events:
        // - CurrentSessionChanged
        // - MediaPropertiesChanged
        // - PlaybackInfoChanged
        // - TimelinePropertiesChanged

        futures::stream::pending() // Placeholder
    }
}

#[async_trait]
impl MediaWatcher for WindowsMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        // TODO: Get all sessions from GetSessions()
        Ok(vec![])
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        // TODO: Find session by source app name
        Err(MediaWatcherError::PlayerNotFound(player_name.to_string()))
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.create_event_stream_impl();
        Ok(Box::pin(stream))
    }
}

// Helper struct to wrap Windows Media Session
struct MediaSession {
    // TODO: Hold GlobalSystemMediaTransportControlsSession
}

impl MediaSession {
    fn to_player_info(&self) -> PlayerInfo {
        // TODO: Extract info from:
        // - TryGetMediaPropertiesAsync() -> title, artist, album
        // - GetPlaybackInfo() -> PlaybackStatus
        // - GetTimelineProperties() -> position, duration

        PlayerInfo::empty("Unknown")
    }
}

fn parse_playback_status(
    status: windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus,
) -> PlaybackState {
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;

    match status {
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => PlaybackState::Playing,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}
