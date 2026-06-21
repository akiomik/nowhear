use std::collections::HashMap;
use std::future::Future;
use std::string::ToString;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use zbus::fdo::DBusProxy;
use zbus::message::Type;
use zbus::names::UniqueName;
use zbus::zvariant::{OwnedValue, Structure, Value};
use zbus::{Connection, Error as ZbusError, MatchRule, Message, MessageStream};

use super::MprisMetadata;
use super::MprisPlaybackStatus;
use crate::{MediaEvent, MediaSourceError, PlaybackState, PlayerInfo, Result, Track};

/// Internal trait for abstracting player discovery mechanisms.
///
/// Used for dependency injection in tests.
pub trait PlayerDiscoveryProvider: Send + Sync {
    /// Discovers all available media players.
    fn discover_players(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    /// Gets information about a specific player.
    fn get_player_info(&self, player_name: &str)
    -> impl Future<Output = Result<PlayerInfo>> + Send;

    /// Creates a stream of media events.
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static;
}

/// MPRIS-based media player provider for Linux.
///
/// Communicates with media players through the D-Bus session bus using the
/// [MPRIS specification](https://specifications.freedesktop.org/mpris-spec/latest/).
pub struct MprisProvider {
    connection: Arc<Connection>,
}

impl MprisProvider {
    // See https://specifications.freedesktop.org/mpris/latest/index.html
    const BUS_NAME_PREFIX: &'static str = "org.mpris.MediaPlayer2.";
    const PLAYER_INTERFACE: &'static str = "org.mpris.MediaPlayer2.Player";
    const OBJECT_PATH: &'static str = "/org/mpris/MediaPlayer2";
    const PROPERTIES_INTERFACE: &'static str = "org.freedesktop.DBus.Properties";
    const PROPERTIES_CHANGED_SIGNAL: &'static str = "PropertiesChanged";
    const NAME_OWNER_CHANGED_SIGNAL: &'static str = "NameOwnerChanged";
    const SEEKED_SIGNAL: &'static str = "Seeked";
    const METADATA_PROPERTY: &'static str = "Metadata";
    const POSITION_PROPERTY: &'static str = "Position";
    const VOLUME_PROPERTY: &'static str = "Volume";
    const PLAYBACK_STATUS_PROPERTY: &'static str = "PlaybackStatus";
    const GET_METHOD: &'static str = "Get";

    /// Creates a new MPRIS provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the D-Bus session connection cannot be established.
    pub async fn new() -> Result<Self> {
        let connection = Connection::session().await?;

        Ok(Self {
            connection: Arc::new(connection),
        })
    }

    /// Initialize the player name cache from the D-Bus session.
    ///
    /// This method queries the D-Bus daemon for all available service names and filters
    /// for MPRIS player names, then retrieves the corresponding unique names.
    ///
    /// # Race Conditions
    ///
    /// Note that there is a potential race condition between the time when we query
    /// the list of names and when we retrieve the unique names. Players could be
    /// added or removed during this initialization process. However, this is acceptable
    /// because:
    ///
    /// - Any players added after initialization will be detected by the `NameOwnerChanged`
    ///   signal handler in the event stream.
    /// - Any players removed during initialization will result in `get_name_owner` returning
    ///   an error, which we handle by skipping that player (via `Ok(owner)` pattern matching).
    /// - The cache is only used as an optimization to map unique names back to player names
    ///   in event handlers, and inconsistencies will be corrected by subsequent events.
    async fn initialize_player_name_cache(
        connection: &Connection,
    ) -> Result<HashMap<UniqueName<'static>, String>> {
        let dbus_proxy = DBusProxy::new(connection).await?;
        let names = dbus_proxy.list_names().await?;
        let mut player_name_cache: HashMap<UniqueName<'static>, String> = HashMap::new();

        for name in names {
            if name.starts_with(Self::BUS_NAME_PREFIX)
                && let Ok(owner) = dbus_proxy.get_name_owner(name.clone().into()).await
            {
                let player_name = Self::extract_player_name(&name);
                player_name_cache.insert(owner.into_inner().into_owned(), player_name);
            }
        }

        Ok(player_name_cache)
    }

    #[inline]
    fn extract_player_name(bus_name: &str) -> String {
        bus_name
            .strip_prefix(Self::BUS_NAME_PREFIX)
            .unwrap_or(bus_name)
            .to_string()
    }

    #[inline]
    fn build_bus_name(player_name: &str) -> String {
        format!("{}{player_name}", Self::BUS_NAME_PREFIX)
    }

    async fn property_changed_stream(connection: &Connection) -> Result<MessageStream> {
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(Self::PROPERTIES_INTERFACE)?
            .path(Self::OBJECT_PATH)?
            .member(Self::PROPERTIES_CHANGED_SIGNAL)?
            .build();
        Ok(MessageStream::for_match_rule(rule, connection, None).await?)
    }

    async fn name_owner_changed_stream(connection: &Connection) -> Result<MessageStream> {
        let rule = zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member(Self::NAME_OWNER_CHANGED_SIGNAL)?
            .build();
        Ok(MessageStream::for_match_rule(rule, connection, None).await?)
    }

    async fn seeked_stream(connection: &Connection) -> Result<MessageStream> {
        let rule = zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(Self::PLAYER_INTERFACE)?
            .path(Self::OBJECT_PATH)?
            .member(Self::SEEKED_SIGNAL)?
            .build();
        Ok(MessageStream::for_match_rule(rule, connection, None).await?)
    }

    /// Get playback status from a player via D-Bus.
    async fn get_playback_state(proxy: &DBusProxy<'_>, player_name: &str) -> Result<PlaybackState> {
        let value: OwnedValue = proxy
            .inner()
            .call(
                Self::GET_METHOD,
                &(Self::PLAYER_INTERFACE, Self::PLAYBACK_STATUS_PROPERTY),
            )
            .await
            .map_err(|e| {
                // If the player doesn't exist, D-Bus will return a ServiceUnknown error
                match &e {
                    ZbusError::MethodError(name, _, _)
                        if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
                    {
                        MediaSourceError::PlayerNotFound(player_name.to_string())
                    }
                    _ => e.into(),
                }
            })?;

        if let Value::Str(status_string) = value.into() {
            Ok(MprisPlaybackStatus::try_from(status_string.as_ref())?.into())
        } else {
            Err(MediaSourceError::ParseError(
                "Failed to get PlaybackStatus".to_string(),
            ))
        }
    }

    /// Get metadata from a player via D-Bus.
    ///
    /// Returns `None` if the property cannot be retrieved or parsed.
    async fn get_metadata(proxy: &DBusProxy<'_>) -> Option<Track> {
        let value: OwnedValue = proxy
            .inner()
            .call(
                Self::GET_METHOD,
                &(Self::PLAYER_INTERFACE, Self::METADATA_PROPERTY),
            )
            .await
            .ok()?;

        if let Value::Dict(dict) = value.into() {
            MprisMetadata::try_from(dict).ok().map(Into::into)
        } else {
            None
        }
    }

    /// Get playback position from a player via D-Bus.
    ///
    /// Returns `None` if the property cannot be retrieved or is negative.
    async fn get_position(proxy: &DBusProxy<'_>) -> Option<Duration> {
        let value: OwnedValue = proxy
            .inner()
            .call(
                Self::GET_METHOD,
                &(Self::PLAYER_INTERFACE, Self::POSITION_PROPERTY),
            )
            .await
            .ok()?;

        if let Value::I64(pos) = value.into() {
            pos.try_into().map(Duration::from_micros).ok()
        } else {
            None
        }
    }

    /// Get volume from a player via D-Bus.
    ///
    /// Returns `None` if the property cannot be retrieved.
    async fn get_volume(proxy: &DBusProxy<'_>) -> Option<f64> {
        let value: OwnedValue = proxy
            .inner()
            .call(
                Self::GET_METHOD,
                &(Self::PLAYER_INTERFACE, Self::VOLUME_PROPERTY),
            )
            .await
            .ok()?;

        if let Value::F64(vol) = value.into() {
            Some(vol)
        } else {
            None
        }
    }

    fn handle_property_changed_message(
        msg: &Message,
        player_name_cache: &HashMap<UniqueName<'static>, String>,
    ) -> Result<Vec<MediaEvent>> {
        let mut events = vec![];

        let body = msg.body();
        let body: Structure = body
            .deserialize()
            .map_err(|e| MediaSourceError::ParseError(e.to_string()))?;

        if let [_, Value::Dict(changed_props), _] = body.fields() {
            let sender = msg.header().sender().cloned().ok_or_else(|| {
                MediaSourceError::InternalError("Failed to get sender".to_string())
            })?;
            // A signal can arrive before the `NameOwnerChanged` that registers the player in the
            // cache (the `select!` in `create_event_stream` picks a ready branch non-deterministically),
            // or just after the player was removed and its cache entry dropped. Such a signal cannot
            // be attributed to a player, so skip it rather than terminating the whole event stream.
            let Some(player_name) = player_name_cache.get(&sender.into_owned()).cloned() else {
                return Ok(events);
            };

            let status_key = Value::new(Self::PLAYBACK_STATUS_PROPERTY);
            if let Some(Value::Str(status)) = changed_props.get(&status_key)? {
                let status = MprisPlaybackStatus::try_from(status)?;
                events.push(MediaEvent::StateChanged {
                    player_name: player_name.clone(),
                    state: status.into(),
                });
            }

            let metadata_key = Value::new(Self::METADATA_PROPERTY);
            let maybe_metadata = changed_props.get(&metadata_key)?;
            if let Some(Value::Dict(metadata_dict)) = maybe_metadata {
                let metadata = MprisMetadata::try_from(metadata_dict)?;
                events.push(MediaEvent::TrackChanged {
                    player_name: player_name.clone(),
                    track: metadata.into(),
                });
            }

            let volume_key = Value::new(Self::VOLUME_PROPERTY);
            let maybe_volume = changed_props.get(&volume_key)?;
            if let Some(Value::F64(volume)) = maybe_volume {
                events.push(MediaEvent::VolumeChanged {
                    player_name,
                    volume,
                });
            }
        }

        Ok(events)
    }

    fn handle_name_owner_changed_message(
        msg: &Message,
        player_name_cache: &mut HashMap<UniqueName<'static>, String>,
    ) -> Result<Option<MediaEvent>> {
        let (bus_name, old_owner, new_owner): (String, String, String) =
            msg.body()
                .deserialize()
                .map_err(|e| MediaSourceError::ParseError(e.to_string()))?;

        if bus_name.starts_with(Self::BUS_NAME_PREFIX) {
            let player_name = Self::extract_player_name(&bus_name);

            if old_owner.is_empty() && !new_owner.is_empty() {
                let unique_name = UniqueName::from_string_unchecked(new_owner);
                player_name_cache.insert(unique_name.into_owned(), player_name.clone());
                return Ok(Some(MediaEvent::PlayerAdded { player_name }));
            } else if !old_owner.is_empty() && new_owner.is_empty() {
                let unique_name = UniqueName::from_string_unchecked(old_owner);
                player_name_cache.remove(&unique_name);

                return Ok(Some(MediaEvent::PlayerRemoved { player_name }));
            }
        }

        Ok(None)
    }

    fn handle_seeked_message(
        msg: &Message,
        player_name_cache: &HashMap<UniqueName<'static>, String>,
    ) -> Result<Option<MediaEvent>> {
        let sender =
            msg.header().sender().cloned().ok_or_else(|| {
                MediaSourceError::InternalError("Failed to get sender".to_string())
            })?;
        // As in `handle_property_changed_message`, a `Seeked` signal can race ahead of the
        // `NameOwnerChanged` that caches the player, or trail its removal. Skip the unattributable
        // signal instead of tearing down the event stream.
        let Some(player_name) = player_name_cache.get(&sender.into_owned()).cloned() else {
            return Ok(None);
        };

        let position_us: i64 = msg
            .body()
            .deserialize()
            .map_err(|e| MediaSourceError::ParseError(e.to_string()))?;
        let position_us: u64 = position_us
            .try_into()
            .map_err(|_| MediaSourceError::ParseError("Failed to convert position".to_string()))?;
        let position = Duration::from_micros(position_us);

        Ok(Some(MediaEvent::PositionChanged {
            player_name,
            position,
        }))
    }
}

impl PlayerDiscoveryProvider for MprisProvider {
    async fn discover_players(&self) -> Result<Vec<String>> {
        let dbus_proxy = DBusProxy::new(&self.connection).await?;

        let names = dbus_proxy.list_names().await?;

        Ok(names
            .iter()
            .filter(|name| name.starts_with(Self::BUS_NAME_PREFIX))
            .map(|name| Self::extract_player_name(name))
            .collect())
    }

    async fn get_player_info(&self, player_name: &str) -> Result<PlayerInfo> {
        let proxy = DBusProxy::builder(&self.connection)
            .destination(Self::build_bus_name(player_name))?
            .interface(Self::PROPERTIES_INTERFACE)?
            .path(Self::OBJECT_PATH)?
            .build()
            .await?;

        // Get PlaybackStatus first (required - will fail fast if player doesn't exist)
        let playback_state = Self::get_playback_state(&proxy, player_name).await?;

        // Get optional properties in parallel
        let (current_track, position, volume) = tokio::join!(
            Self::get_metadata(&proxy),
            Self::get_position(&proxy),
            Self::get_volume(&proxy)
        );

        Ok(PlayerInfo {
            player_name: player_name.to_string(),
            current_track,
            playback_state,
            position,
            volume,
        })
    }

    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
        let connection = Arc::clone(&self.connection);
        let (tx, rx) = mpsc::unbounded_channel();

        let _handle: JoinHandle<Result<()>> = tokio::spawn(async move {
            let mut property_changed_stream = Self::property_changed_stream(&connection).await?;
            let mut name_owner_changed_stream =
                Self::name_owner_changed_stream(&connection).await?;
            let mut seeked_stream = Self::seeked_stream(&connection).await?;
            let mut player_name_cache = Self::initialize_player_name_cache(&connection).await?;

            loop {
                tokio::select! {
                    // Stop as soon as the consumer drops the stream, even while every signal stream
                    // is idle. Without this arm the task only notices a dropped receiver when the
                    // next signal happens to arrive and a send fails, so a paused or idle player
                    // would keep this task — and its D-Bus match rules and connection handle —
                    // alive indefinitely.
                    () = tx.closed() => break,
                    Some(msg) = property_changed_stream.next() => {
                        // A single undecodable message (stream-level error) or one that fails to
                        // parse must not tear down the whole stream; skip it and keep monitoring.
                        // Each poll advances past the offending message, so this cannot spin.
                        if let Ok(msg) = msg
                            && let Ok(events) = Self::handle_property_changed_message(&msg, &player_name_cache)
                        {
                            for event in events {
                                if tx.send(event).is_err() {
                                    // Receiver dropped mid-batch: tear down the whole task. A plain
                                    // `break` here would only exit the `for`, leaving the outer loop
                                    // spinning until another signal arrived.
                                    return Ok(());
                                }
                            }
                        }
                    }
                    Some(msg) = name_owner_changed_stream.next() => {
                        if let Ok(msg) = msg
                            && let Ok(Some(event)) = Self::handle_name_owner_changed_message(&msg, &mut player_name_cache)
                            && tx.send(event).is_err()
                        {
                            break;
                        }
                    }
                    Some(msg) = seeked_stream.next() => {
                        if let Ok(msg) = msg
                            && let Ok(Some(event)) = Self::handle_seeked_message(&msg, &player_name_cache)
                            && tx.send(event).is_err()
                        {
                            break;
                        }
                    }
                    else => break,
                }
            }

            Ok(())
        });

        UnboundedReceiverStream::new(rx)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::result::Result;

    use crate::{PlaybackState, Track};

    use super::*;

    #[test]
    fn test_mpris_provider_extract_player_name_with_prefix() {
        assert_eq!(
            MprisProvider::extract_player_name("org.mpris.MediaPlayer2.spotify"),
            "spotify"
        );
        assert_eq!(
            MprisProvider::extract_player_name("org.mpris.MediaPlayer2.vlc"),
            "vlc"
        );
        assert_eq!(
            MprisProvider::extract_player_name("org.mpris.MediaPlayer2.rhythmbox"),
            "rhythmbox"
        );
    }

    #[test]
    fn test_mpris_provider_extract_player_name_without_prefix() {
        assert_eq!(
            MprisProvider::extract_player_name("custom.player"),
            "custom.player"
        );
        assert_eq!(MprisProvider::extract_player_name("player"), "player");
    }

    #[test]
    fn test_mpris_provider_extract_player_name_with_instance() {
        // Some players add instance numbers
        assert_eq!(
            MprisProvider::extract_player_name("org.mpris.MediaPlayer2.chromium.instance1234"),
            "chromium.instance1234"
        );
    }

    #[test]
    fn test_handle_property_changed_message() -> Result<(), Box<dyn Error>> {
        let mut player_name_cache = HashMap::new();
        player_name_cache.insert(UniqueName::from_str_unchecked(":foo.bar"), "Foo".to_owned());
        let mut metadata: HashMap<&str, Value> = HashMap::new();
        metadata.insert("xesam:title", Value::new("Test Song"));
        let mut changed_properties: HashMap<&str, Value> = HashMap::new();
        changed_properties.insert(
            MprisProvider::PLAYBACK_STATUS_PROPERTY,
            Value::new("Playing"),
        );
        changed_properties.insert(MprisProvider::VOLUME_PROPERTY, Value::F64(0.42));
        changed_properties.insert(MprisProvider::METADATA_PROPERTY, Value::new(metadata));
        let msg = Message::signal(
            MprisProvider::OBJECT_PATH,
            MprisProvider::PROPERTIES_INTERFACE,
            MprisProvider::PROPERTIES_CHANGED_SIGNAL,
        )?
        .sender(":foo.bar")?
        .build(&(
            MprisProvider::PLAYER_INTERFACE,
            changed_properties,
            Vec::<&str>::new(),
        ))?;

        let events = MprisProvider::handle_property_changed_message(&msg, &player_name_cache)?;

        assert_eq!(
            events,
            vec![
                MediaEvent::StateChanged {
                    player_name: "Foo".to_string(),
                    state: PlaybackState::Playing,
                },
                MediaEvent::TrackChanged {
                    player_name: "Foo".to_string(),
                    track: Track {
                        title: "Test Song".to_string(),
                        artist: vec![],
                        album: None,
                        album_artist: vec![],
                        track_number: None,
                        duration: None,
                        art_url: None,
                    }
                },
                MediaEvent::VolumeChanged {
                    player_name: "Foo".to_string(),
                    volume: 0.42,
                }
            ]
        );

        Ok(())
    }

    #[test]
    fn test_handle_property_changed_message_unknown_player() -> Result<(), Box<dyn Error>> {
        // The sender is not in the cache (e.g. a signal racing ahead of NameOwnerChanged, or
        // trailing a removal). It must be skipped, not surfaced as an error that kills the stream.
        let player_name_cache = HashMap::new();
        let mut changed_properties: HashMap<&str, Value> = HashMap::new();
        changed_properties.insert(
            MprisProvider::PLAYBACK_STATUS_PROPERTY,
            Value::new("Playing"),
        );
        let msg = Message::signal(
            MprisProvider::OBJECT_PATH,
            MprisProvider::PROPERTIES_INTERFACE,
            MprisProvider::PROPERTIES_CHANGED_SIGNAL,
        )?
        .sender(":unknown.player")?
        .build(&(
            MprisProvider::PLAYER_INTERFACE,
            changed_properties,
            Vec::<&str>::new(),
        ))?;

        let events = MprisProvider::handle_property_changed_message(&msg, &player_name_cache)?;

        assert_eq!(events, vec![]);

        Ok(())
    }

    #[test]
    fn test_handle_name_owner_changed_message_added() -> Result<(), Box<dyn Error>> {
        let mut player_name_cache = HashMap::new();
        let msg = Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            MprisProvider::NAME_OWNER_CHANGED_SIGNAL,
        )?
        .sender("org.freedesktop.DBus")?
        .build(&("org.mpris.MediaPlayer2.Foo", "", ":foo.bar"))?;

        let event = MprisProvider::handle_name_owner_changed_message(&msg, &mut player_name_cache)?;

        assert_eq!(
            event,
            Some(MediaEvent::PlayerAdded {
                player_name: "Foo".to_string(),
            })
        );
        assert_eq!(player_name_cache.get(":foo.bar"), Some(&"Foo".to_string()));

        Ok(())
    }

    #[test]
    fn test_handle_name_owner_changed_message_removed() -> Result<(), Box<dyn Error>> {
        let mut player_name_cache = HashMap::new();
        player_name_cache.insert(
            UniqueName::from_str_unchecked(":foo.bar"),
            "Foo".to_string(),
        );
        let msg = Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            MprisProvider::NAME_OWNER_CHANGED_SIGNAL,
        )?
        .sender("org.freedesktop.DBus")?
        .build(&("org.mpris.MediaPlayer2.Foo", ":foo.bar", ""))?;

        let event = MprisProvider::handle_name_owner_changed_message(&msg, &mut player_name_cache)?;

        assert_eq!(
            event,
            Some(MediaEvent::PlayerRemoved {
                player_name: "Foo".to_string(),
            })
        );
        assert_eq!(player_name_cache.get(":foo.bar"), None);

        Ok(())
    }

    #[test]
    fn test_handle_seeked_message() -> Result<(), Box<dyn Error>> {
        let mut player_name_cache = HashMap::new();
        player_name_cache.insert(
            UniqueName::from_str_unchecked(":foo.bar"),
            "Foo".to_string(),
        );
        let msg = Message::signal(
            MprisProvider::OBJECT_PATH,
            MprisProvider::PLAYER_INTERFACE,
            MprisProvider::SEEKED_SIGNAL,
        )?
        .sender(":foo.bar")?
        .build(&(180_000_000_i64))?;

        let event = MprisProvider::handle_seeked_message(&msg, &player_name_cache)?;

        assert_eq!(
            event,
            Some(MediaEvent::PositionChanged {
                player_name: "Foo".to_string(),
                position: Duration::from_secs(180)
            })
        );

        Ok(())
    }

    #[test]
    fn test_handle_seeked_message_unknown_player() -> Result<(), Box<dyn Error>> {
        // The sender is not in the cache; the signal must be skipped rather than terminating the
        // event stream.
        let player_name_cache = HashMap::new();
        let msg = Message::signal(
            MprisProvider::OBJECT_PATH,
            MprisProvider::PLAYER_INTERFACE,
            MprisProvider::SEEKED_SIGNAL,
        )?
        .sender(":unknown.player")?
        .build(&(180_000_000_i64))?;

        let event = MprisProvider::handle_seeked_message(&msg, &player_name_cache)?;

        assert_eq!(event, None);

        Ok(())
    }
}
