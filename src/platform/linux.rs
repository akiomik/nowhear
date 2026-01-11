#[cfg(target_os = "linux")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::Connection;

pub struct LinuxMediaWatcher {
    connection: Connection,
    // Cache of known players
    players: Arc<RwLock<HashMap<String, PlayerInfo>>>,
}

impl LinuxMediaWatcher {
    pub async fn new() -> Result<Self> {
        let connection = Connection::session()
            .await
            .map_err(|e| MediaWatcherError::ConnectionError(e.to_string()))?;

        Ok(Self {
            connection,
            players: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    async fn discover_players(&self) -> Result<Vec<String>> {
        // MPRIS players have names like "org.mpris.MediaPlayer2.spotify"
        // Use D-Bus introspection to find all MPRIS players

        // TODO: Implement D-Bus discovery
        // This would use zbus to list all services matching "org.mpris.MediaPlayer2.*"

        Ok(vec![])
    }

    async fn get_player_info(&self, player_name: &str) -> Result<PlayerInfo> {
        // TODO: Connect to the player's MPRIS interface and query:
        // - org.mpris.MediaPlayer2.Player.Metadata
        // - org.mpris.MediaPlayer2.Player.PlaybackStatus
        // - org.mpris.MediaPlayer2.Player.Position
        // - org.mpris.MediaPlayer2.Player.Volume

        Ok(PlayerInfo::empty(player_name))
    }

    fn create_event_stream_impl(&self) -> impl Stream<Item = MediaEvent> {
        // TODO: Subscribe to D-Bus signals:
        // - PropertiesChanged on org.mpris.MediaPlayer2.Player
        // - NameOwnerChanged to detect player appearance/disappearance

        // Return a stream that yields MediaEvent items
        futures::stream::pending() // Placeholder
    }
}

#[async_trait]
impl MediaWatcher for LinuxMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        self.discover_players().await
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        self.get_player_info(player_name).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.create_event_stream_impl();
        Ok(Box::pin(stream))
    }
}

// Helper functions to convert MPRIS data to our types
fn parse_metadata(metadata: &HashMap<String, zbus::zvariant::OwnedValue>) -> Track {
    // TODO: Extract title, artist, album, etc. from MPRIS metadata
    Track::unknown()
}

fn parse_playback_status(status: &str) -> PlaybackState {
    match status {
        "Playing" => PlaybackState::Playing,
        "Paused" => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}
