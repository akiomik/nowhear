//! Windows-specific implementation using Windows Media Control API.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use futures::future;
use futures::stream::Stream;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::UnboundedReceiverStream;
use windows::Foundation::{IStringable, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as WinSession,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as WinMediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as WinPlaybackStatus,
};
use windows::core::Interface;

use crate::error::{MediaSourceError, Result};
use crate::source::{EventStream, MediaSource};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};

/// Upper bound on how long a single `get_session_state` read may take.
///
/// The session-discovery path reads state while holding the monitor lock, so a player whose
/// WinRT async call never resolves would otherwise block all further session updates. Bounding
/// the read caps that stall and lets discovery of other players proceed.
const SESSION_STATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Internal player state representation for Windows implementation.
///
/// This structure is used internally to track player state changes and
/// is not part of the public API.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub track: Track,
    pub playback_state: PlaybackState,
    pub position: Option<Duration>,
    pub volume: Option<f64>,
}

/// Windows media session with its identifier.
///
/// This structure pairs a session ID with its corresponding Windows session object,
/// avoiding redundant ID extraction operations.
struct SessionWithId {
    id: String,
    session: WinSession,
}

/// Cached playback state shared across all event handlers for a single session.
struct SessionState {
    track: Option<Track>,
    position: Option<Duration>,
    playback_state: PlaybackState,
}

/// Per-session state and event handler tokens for event-driven monitoring.
///
/// The Drop impl sets `removed = true` first (so in-flight spawned tasks see it) then
/// deregisters all registered event handlers, preventing new WinRT callbacks after removal.
/// Tokens are `Option<i64>`: `None` means the handler was never registered (e.g., due
/// to a transient API error); Drop skips deregistration in that case.
struct SessionEntry {
    session: WinSession,
    removed: Arc<AtomicBool>,
    token_media_props: Option<i64>,
    token_playback_info: Option<i64>,
    token_timeline: Option<i64>,
}

impl SessionEntry {
    const fn is_fully_registered(&self) -> bool {
        self.token_media_props.is_some()
            && self.token_playback_info.is_some()
            && self.token_timeline.is_some()
    }
}

impl Drop for SessionEntry {
    fn drop(&mut self) {
        // Signal removed before deregistering so already-queued spawned tasks skip sending.
        self.removed.store(true, Ordering::Release);
        if let Some(token) = self.token_media_props {
            let _ = self.session.RemoveMediaPropertiesChanged(token);
        }
        if let Some(token) = self.token_playback_info {
            let _ = self.session.RemovePlaybackInfoChanged(token);
        }
        if let Some(token) = self.token_timeline {
            let _ = self.session.RemoveTimelinePropertiesChanged(token);
        }
    }
}

/// RAII guard that deregisters the manager-level `SessionsChanged` handler on drop.
///
/// Holding the registration in a guard ensures the handler is removed even if the monitor task
/// panics or is cancelled before its normal teardown, so the Windows OS never keeps firing
/// `SessionsChanged` callbacks into a closure whose state has been orphaned.
struct SessionsChangedGuard {
    manager: SessionManager,
    token: i64,
}

impl Drop for SessionsChangedGuard {
    fn drop(&mut self) {
        let _ = self.manager.RemoveSessionsChanged(self.token);
    }
}

/// Internal trait for abstracting media session access.
///
/// This trait is used internally by the Windows implementation to allow
/// for dependency injection in tests. It is not part of the public API.
pub trait MediaSessionProvider: Send + Sync {
    fn get_all_sessions(&self)
    -> impl Future<Output = Result<HashMap<String, PlayerState>>> + Send;
    fn get_session_info(&self, session_id: &str)
    -> impl Future<Output = Result<PlayerInfo>> + Send;
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static;
}

/// Windows Media Control provider.
///
/// This provider uses the Windows Media Control API to query media sessions.
pub struct WindowsMediaControlProvider {
    manager: SessionManager,
}

impl WindowsMediaControlProvider {
    pub async fn new() -> Result<Self> {
        let manager = SessionManager::RequestAsync()
            .map_err(|e| {
                MediaSourceError::ConnectionError(format!("Failed to get session manager: {e}"))
            })?
            .await
            .map_err(|e| {
                MediaSourceError::ConnectionError(format!("Failed to await session manager: {e}"))
            })?;

        Ok(Self { manager })
    }

    /// Enumerates the raw session objects from a session manager.
    ///
    /// Sessions that fail to fetch via `GetAt` are skipped; an error is only returned when the
    /// session list itself or its size cannot be read.
    fn collect_sessions(manager: &SessionManager) -> Result<Vec<WinSession>> {
        let sessions = manager.GetSessions().map_err(|e| {
            MediaSourceError::ConnectionError(format!("Failed to get sessions: {e}"))
        })?;

        let size = sessions.Size().map_err(|e| {
            MediaSourceError::ConnectionError(format!("Failed to get sessions size: {e}"))
        })?;

        let mut result = Vec::new();
        for i in 0..size {
            if let Ok(session) = sessions.GetAt(i) {
                result.push(session);
            }
        }

        Ok(result)
    }

    fn get_sessions(&self) -> Result<Vec<WinSession>> {
        Self::collect_sessions(&self.manager)
    }

    fn get_session_id(session: &WinSession) -> Result<String> {
        let app_id = session
            .SourceAppUserModelId()
            .map_err(|e| MediaSourceError::ParseError(format!("Failed to get app ID: {e}")))?;

        Ok(app_id.to_string())
    }

    fn build_track(media_props: &WinMediaProperties, duration: Option<Duration>) -> Track {
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

    /// Fetches only the track metadata for a session, without reading playback info.
    ///
    /// Used in the `MediaPropertiesChanged` handler so that a transient failure in
    /// `GetPlaybackInfo` or `GetTimelineProperties` does not silently drop a track-change
    /// event. `Track.duration` is populated from `EndTime` if available but is not required.
    async fn get_track_from_session(session: &WinSession) -> Result<Track> {
        let media_props = session
            .TryGetMediaPropertiesAsync()
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to get media properties: {e}"))
            })?
            .await
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to await media properties: {e}"))
            })?;

        let duration = session
            .GetTimelineProperties()
            .ok()
            .and_then(|t| t.EndTime().ok())
            .filter(|t| t.Duration > 0)
            .and_then(|t| ticks_to_duration(t.Duration));

        Ok(Self::build_track(&media_props, duration))
    }

    async fn get_session_state(session: &WinSession) -> Result<PlayerState> {
        let media_props = session
            .TryGetMediaPropertiesAsync()
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to get media properties: {e}"))
            })?
            .await
            .map_err(|e| {
                MediaSourceError::ParseError(format!("Failed to await media properties: {e}"))
            })?;

        let playback_info = session.GetPlaybackInfo().map_err(|e| {
            MediaSourceError::ParseError(format!("Failed to get playback info: {e}"))
        })?;

        let playback_status = playback_info.PlaybackStatus().map_err(|e| {
            MediaSourceError::ParseError(format!("Failed to get playback status: {e}"))
        })?;

        let timeline = session
            .GetTimelineProperties()
            .map_err(|e| MediaSourceError::ParseError(format!("Failed to get timeline: {e}")))?;

        let position = timeline
            .Position()
            .ok()
            .and_then(|t| ticks_to_duration(t.Duration));
        let duration = timeline
            .EndTime()
            .ok()
            .filter(|t| t.Duration > 0)
            .and_then(|t| ticks_to_duration(t.Duration));

        Ok(PlayerState {
            track: Self::build_track(&media_props, duration),
            playback_state: parse_playback_status(playback_status),
            position,
            volume: None,
        })
    }

    /// [`get_session_state`](Self::get_session_state) bounded by [`SESSION_STATE_TIMEOUT`].
    ///
    /// A hung player whose WinRT async read never resolves would otherwise stall the monitor
    /// lock indefinitely; on timeout this returns an error so the caller skips the session and
    /// retries on the next `SessionsChanged` event.
    async fn get_session_state_bounded(session: &WinSession) -> Result<PlayerState> {
        timeout(SESSION_STATE_TIMEOUT, Self::get_session_state(session))
            .await
            .unwrap_or_else(|_| {
                Err(MediaSourceError::ConnectionError(
                    "Timed out reading session state".to_string(),
                ))
            })
    }

    fn get_sessions_from_manager(manager: &SessionManager) -> Result<Vec<SessionWithId>> {
        let result = Self::collect_sessions(manager)?
            .into_iter()
            .filter_map(|session| {
                Self::get_session_id(&session)
                    .ok()
                    .map(|id| SessionWithId { id, session })
            })
            .collect();

        Ok(result)
    }
}

impl MediaSessionProvider for WindowsMediaControlProvider {
    async fn get_all_sessions(&self) -> Result<HashMap<String, PlayerState>> {
        let sessions = self.get_sessions()?;
        let mut session_states = HashMap::new();

        for session in sessions {
            if let Ok(session_id) = Self::get_session_id(&session)
                && let Ok(state) = Self::get_session_state(&session).await
            {
                session_states.insert(session_id, state);
            }
        }

        Ok(session_states)
    }

    async fn get_session_info(&self, session_id: &str) -> Result<PlayerInfo> {
        let sessions = self.get_sessions()?;

        for session in sessions {
            let id = Self::get_session_id(&session)?;
            if id == session_id {
                let state = Self::get_session_state(&session).await?;
                return Ok(PlayerInfo {
                    player_name: session_id.to_string(),
                    current_track: Some(state.track),
                    playback_state: state.playback_state,
                    position: state.position,
                    volume: state.volume,
                });
            }
        }

        Err(MediaSourceError::PlayerNotFound(session_id.to_string()))
    }

    // The SessionsChanged/initial-scan tasks deliberately hold the monitor guard across the
    // session-list snapshot to preserve monotonic ordering, which trips significant_drop_tightening.
    #[allow(clippy::significant_drop_tightening)]
    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
        let manager = self.manager.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let monitor = Arc::new(Mutex::new(EventDrivenPlayerMonitor::new(tx.clone())));

            // Register SessionsChanged BEFORE the initial scan so that any session change
            // during the scan window is queued and processed after the scan completes.
            // The Mutex on EventDrivenPlayerMonitor serialises the two update_sessions calls.
            let manager_clone = manager.clone();
            let monitor_clone = monitor.clone();
            let handle = Handle::current();
            let sessions_changed_guard = manager
                .SessionsChanged(&TypedEventHandler::new(move |_sender, _args| {
                    let manager = manager_clone.clone();
                    let monitor = monitor_clone.clone();
                    let handle = handle.clone();

                    // Acquire the monitor lock BEFORE snapshotting the session list. Taking the
                    // snapshot under the lock makes concurrent SessionsChanged tasks observe the
                    // list in the same order they apply it, so an older snapshot can never
                    // overwrite a newer one and spuriously emit PlayerRemoved for a live session.
                    // We always fetch the full list rather than trusting the event args.
                    handle.spawn(async move {
                        // The guard is intentionally acquired before the snapshot and held across
                        // it; tightening the lock scope would reintroduce the stale-snapshot race.
                        let mut guard = monitor.lock().await;
                        if let Ok(sessions_with_ids) = Self::get_sessions_from_manager(&manager) {
                            guard.update_sessions(sessions_with_ids).await;
                        }
                    });
                    Ok(())
                }))
                .ok()
                .map(|token| SessionsChangedGuard {
                    manager: manager.clone(),
                    token,
                });

            // Initial session scan — retry up to 3 times on transient failure.
            // Without retry, a player already running at startup is invisible until
            // the next SessionsChanged event fires.
            // TODO: if all 3 attempts fail and no SessionsChanged fires afterward (i.e. only
            // one player is running and it never changes), already-running sessions will remain
            // invisible for the entire stream lifetime. Polling at a long interval as a fallback
            // would close this gap, but reintroduces the complexity that the event-driven
            // architecture was designed to eliminate.
            for attempt in 0..3u32 {
                // Snapshot under the lock for the same monotonic-ordering reason as the
                // SessionsChanged handler above.
                let mut guard = monitor.lock().await;
                if let Ok(sessions_with_ids) = Self::get_sessions_from_manager(&manager) {
                    guard.update_sessions(sessions_with_ids).await;
                    break;
                }
                drop(guard);
                if attempt < 2 {
                    sleep(Duration::from_millis(500)).await;
                }
            }

            // Wait until the consumer drops the stream, then deregister SessionsChanged by
            // dropping the guard. The guard also deregisters during unwinding if this task panics
            // or is cancelled before reaching this point, so a callback can never fire into an
            // orphaned closure.
            tx.closed().await;
            drop(sessions_changed_guard);
        });

        UnboundedReceiverStream::new(rx)
    }
}

/// Windows media source implementation using Windows Media Control API.
///
/// This implementation uses the Windows Runtime API
/// [`GlobalSystemMediaTransportControlsSessionManager`](https://learn.microsoft.com/en-us/uwp/api/windows.media.control.globalsystemmediatransportcontrolssessionmanager)
/// to interact with media players on Windows 10 and later.
///
/// It supports any application that integrates with Windows Media Control, including:
///
/// - Spotify
/// - VLC
/// - Windows Media Player
/// - Microsoft Edge (for web-based media)
/// - Chrome (for web-based media)
/// - And many more
///
/// # Implementation Details
///
/// The implementation is fully event-driven using Windows Runtime event handlers:
/// - `SessionsChanged`: Detects new or removed media players
/// - `MediaPropertiesChanged`: Detects track changes
/// - `PlaybackInfoChanged`: Detects playback state changes (play/pause/stop)
/// - `TimelinePropertiesChanged`: Detects position changes (seeking)
///
/// This provides real-time updates with minimal resource usage and no polling overhead.
///
/// # Note
///
/// This type is visible for technical reasons but should not be used directly.
/// Use [`nowhear::MediaSourceBuilder`] to create media sources, which will
/// automatically select this implementation on Windows systems.
pub struct WindowsMediaSource<P: MediaSessionProvider = WindowsMediaControlProvider> {
    provider: Arc<P>,
}

/// A newly discovered session whose handlers have been registered but whose authoritative
/// state has not yet been read and emitted.
struct PendingSession {
    id: String,
    entry: SessionEntry,
    session_state: Arc<StdMutex<SessionState>>,
    ready: Arc<AtomicBool>,
}

/// Event-driven player monitor using Windows Runtime event handlers.
struct EventDrivenPlayerMonitor {
    sessions: HashMap<String, SessionEntry>,
    event_tx: mpsc::UnboundedSender<MediaEvent>,
}

impl EventDrivenPlayerMonitor {
    fn new(event_tx: mpsc::UnboundedSender<MediaEvent>) -> Self {
        Self {
            sessions: HashMap::new(),
            event_tx,
        }
    }

    async fn update_sessions(&mut self, sessions_with_ids: Vec<SessionWithId>) {
        let current_ids: HashSet<_> = sessions_with_ids.iter().map(|s| s.id.clone()).collect();
        let previous_ids: HashSet<_> = self.sessions.keys().cloned().collect();

        // Remove stale sessions. SessionEntry::drop sets removed=true then deregisters handlers.
        for removed_id in previous_ids.difference(&current_ids) {
            self.sessions.remove(removed_id.as_str());
            let _ = self.event_tx.send(MediaEvent::PlayerRemoved {
                player_name: removed_id.clone(),
            });
        }

        // Collect sessions not yet tracked, then register their handlers up front. Registration
        // is synchronous and performs no I/O, so a handler can start observing changes
        // immediately. The authoritative state is read once per session afterwards (below) in a
        // single parallel batch: reading after registration means no change can slip through an
        // unwatched window, and reading only once avoids the redundant second round-trip a
        // pre-registration read would otherwise require.
        let new_sessions: Vec<SessionWithId> = sessions_with_ids
            .into_iter()
            .filter(|s| !self.sessions.contains_key(&s.id))
            .collect();

        let mut pending: Vec<PendingSession> = Vec::new();
        for SessionWithId { id, session } in new_sessions {
            // `ready` gates handler emissions until PlayerAdded has been sent, so a handler
            // firing during this setup cannot push TrackChanged/StateChanged/PositionChanged
            // into the channel ahead of PlayerAdded. The cache starts empty; the authoritative
            // read below fills it before any event is emitted.
            let ready = Arc::new(AtomicBool::new(false));
            let session_state = Arc::new(StdMutex::new(SessionState {
                track: None,
                position: None,
                playback_state: PlaybackState::Stopped,
            }));

            let entry = self.register_event_handlers(
                session,
                &id,
                &session_state,
                Arc::new(AtomicBool::new(false)),
                &ready,
            );

            // Keep only sessions whose three handlers all registered. A partial registration is
            // dropped here (SessionEntry::drop deregisters any successful handlers), so the next
            // SessionsChanged event retries.
            if entry.is_fully_registered() {
                pending.push(PendingSession {
                    id,
                    entry,
                    session_state,
                    ready,
                });
            }
        }

        // Read every registered session's state in parallel so a slow or hung player does not
        // block discovery of the others.
        let states = future::join_all(
            pending
                .iter()
                .map(|p| WindowsMediaControlProvider::get_session_state_bounded(&p.entry.session)),
        )
        .await;

        for (p, state_result) in pending.into_iter().zip(states) {
            let Ok(state) = state_result else {
                // Read failed or timed out: drop the entry (which deregisters the handlers) and
                // skip, so the next SessionsChanged event retries.
                // FIXME: if no SessionsChanged fires after this (the player is already running
                // and stable), this session is permanently untracked for the lifetime of the
                // stream. A background retry task per failed session would fix this, but adds
                // significant complexity.
                continue;
            };

            {
                let mut cache = p
                    .session_state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                cache.track = Some(state.track.clone());
                cache.position = state.position;
                cache.playback_state = state.playback_state;
            }

            let _ = self.event_tx.send(MediaEvent::PlayerAdded {
                player_name: p.id.clone(),
            });
            let _ = self.event_tx.send(MediaEvent::TrackChanged {
                player_name: p.id.clone(),
                track: state.track,
            });
            let _ = self.event_tx.send(MediaEvent::StateChanged {
                player_name: p.id.clone(),
                state: state.playback_state,
            });
            // PlayerAdded is now queued ahead of any handler event; allow handlers to emit.
            p.ready.store(true, Ordering::Release);
            self.sessions.insert(p.id, p.entry);
        }
    }

    fn register_event_handlers(
        &self,
        session: WinSession,
        session_id: &str,
        session_state: &Arc<StdMutex<SessionState>>,
        removed: Arc<AtomicBool>,
        ready: &Arc<AtomicBool>,
    ) -> SessionEntry {
        let tx = &self.event_tx;

        let token_media_props = Self::register_media_properties_changed(
            &session,
            tx,
            session_id,
            Arc::clone(session_state),
            Arc::clone(&removed),
            Arc::clone(ready),
        );
        let token_playback_info = Self::register_playback_info_changed(
            &session,
            tx,
            session_id,
            Arc::clone(session_state),
            Arc::clone(&removed),
            Arc::clone(ready),
        );
        let token_timeline = Self::register_timeline_properties_changed(
            &session,
            tx,
            session_id,
            Arc::clone(session_state),
            Arc::clone(&removed),
            Arc::clone(ready),
        );

        SessionEntry {
            session,
            removed,
            token_media_props,
            token_playback_info,
            token_timeline,
        }
    }

    fn register_media_properties_changed(
        session: &WinSession,
        tx: &mpsc::UnboundedSender<MediaEvent>,
        player_name: &str,
        session_state: Arc<StdMutex<SessionState>>,
        removed: Arc<AtomicBool>,
        ready: Arc<AtomicBool>,
    ) -> Option<i64> {
        let handle = Handle::current();
        let tx = tx.clone();
        let player_name = player_name.to_string();
        let session_clone = session.clone();

        session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_sender, _args| {
                let handle = handle.clone();
                let tx = tx.clone();
                let player_name = player_name.clone();
                let session = session_clone.clone();
                let session_state = Arc::clone(&session_state);
                let removed = Arc::clone(&removed);
                let ready = Arc::clone(&ready);

                handle.spawn(async move {
                    if let Ok(track) =
                        WindowsMediaControlProvider::get_track_from_session(&session).await
                    {
                        let mut state =
                            session_state.lock().unwrap_or_else(PoisonError::into_inner);
                        if track_identity_changed(state.track.as_ref(), &track) {
                            state.track = Some(track.clone());
                            // Reset position atomically with the track update so that
                            // any concurrent TimelinePropertiesChanged task that acquires
                            // this lock after us sees position=None only after TrackChanged
                            // is already in the channel (sent below while still holding the lock).
                            state.position = None;
                            // `ready` keeps emissions ordered after PlayerAdded for a new session.
                            if ready.load(Ordering::Acquire) && !removed.load(Ordering::Acquire) {
                                let _ = tx.send(MediaEvent::TrackChanged { player_name, track });
                            }
                        } else {
                            // Metadata-only change (e.g. duration or art_url loaded late):
                            // update the cache without emitting TrackChanged.
                            state.track = Some(track);
                        }
                        // Lock releases here; position=None is now visible to timeline handler.
                    }
                });
                Ok(())
            }))
            .ok()
    }

    fn register_playback_info_changed(
        session: &WinSession,
        tx: &mpsc::UnboundedSender<MediaEvent>,
        player_name: &str,
        session_state: Arc<StdMutex<SessionState>>,
        removed: Arc<AtomicBool>,
        ready: Arc<AtomicBool>,
    ) -> Option<i64> {
        let handle = Handle::current();
        let tx = tx.clone();
        let player_name = player_name.to_string();
        let session_clone = session.clone();

        session
            .PlaybackInfoChanged(&TypedEventHandler::new(move |_sender, _args| {
                let handle = handle.clone();
                let tx = tx.clone();
                let player_name = player_name.clone();
                let session = session_clone.clone();
                let session_state = Arc::clone(&session_state);
                let removed = Arc::clone(&removed);
                let ready = Arc::clone(&ready);

                handle.spawn(async move {
                    if let Ok(playback_info) = session.GetPlaybackInfo()
                        && let Ok(status) = playback_info.PlaybackStatus()
                    {
                        let new_state = parse_playback_status(status);
                        let mut state =
                            session_state.lock().unwrap_or_else(PoisonError::into_inner);
                        if state.playback_state != new_state {
                            state.playback_state = new_state;
                            drop(state);
                            // `ready` keeps emissions ordered after PlayerAdded for a new session.
                            if ready.load(Ordering::Acquire) && !removed.load(Ordering::Acquire) {
                                let _ = tx.send(MediaEvent::StateChanged {
                                    player_name,
                                    state: new_state,
                                });
                            }
                        }
                    }
                });
                Ok(())
            }))
            .ok()
    }

    fn register_timeline_properties_changed(
        session: &WinSession,
        tx: &mpsc::UnboundedSender<MediaEvent>,
        player_name: &str,
        session_state: Arc<StdMutex<SessionState>>,
        removed: Arc<AtomicBool>,
        ready: Arc<AtomicBool>,
    ) -> Option<i64> {
        let handle = Handle::current();
        let tx = tx.clone();
        let player_name = player_name.to_string();
        let session_clone = session.clone();

        session
            .TimelinePropertiesChanged(&TypedEventHandler::new(move |_sender, _args| {
                let handle = handle.clone();
                let tx = tx.clone();
                let player_name = player_name.clone();
                let session = session_clone.clone();
                let session_state = Arc::clone(&session_state);
                let removed = Arc::clone(&removed);
                let ready = Arc::clone(&ready);

                handle.spawn(async move {
                    let Some(position) = session
                        .GetTimelineProperties()
                        .ok()
                        .and_then(|t| t.Position().ok())
                        .and_then(|t| ticks_to_duration(t.Duration))
                    else {
                        return;
                    };

                    let should_emit = {
                        let mut state =
                            session_state.lock().unwrap_or_else(PoisonError::into_inner);
                        // `ready` keeps emissions ordered after PlayerAdded for a new session.
                        if is_seek(state.position, position)
                            && ready.load(Ordering::Acquire)
                            && !removed.load(Ordering::Acquire)
                        {
                            state.position = Some(position);
                            true
                        } else {
                            false
                        }
                    };
                    if should_emit {
                        let _ = tx.send(MediaEvent::PositionChanged {
                            player_name,
                            position,
                        });
                    }
                });
                Ok(())
            }))
            .ok()
    }
}

impl WindowsMediaSource<WindowsMediaControlProvider> {
    /// Creates a new Windows media source.
    ///
    /// Note: This is an internal API. Use `MediaSourceBuilder` instead.
    pub async fn new() -> Result<Self> {
        Ok(Self {
            provider: Arc::new(WindowsMediaControlProvider::new().await?),
        })
    }
}

impl<P: MediaSessionProvider + 'static> WindowsMediaSource<P> {
    #[cfg(test)]
    pub const fn with_provider(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

impl<P: MediaSessionProvider + 'static> MediaSource for WindowsMediaSource<P> {
    async fn list_players(&self) -> Result<Vec<String>> {
        let sessions = self.provider.get_all_sessions().await?;
        Ok(sessions.keys().cloned().collect())
    }

    async fn get_player(&self, player_name: impl AsRef<str> + Send) -> Result<PlayerInfo> {
        self.provider.get_session_info(player_name.as_ref()).await
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = self.provider.create_event_stream();
        Ok(Box::pin(stream))
    }
}

/// Converts a Windows `TimeSpan` tick value (100-nanosecond units, i64) to a `Duration`.
///
/// Returns `None` for negative values (Windows sentinel for unavailable positions) and for
/// values that would overflow `u64` when multiplied by 100 (e.g. `i64::MAX`, which Windows
/// uses as a sentinel for unknown/infinite duration in live streams).
#[allow(clippy::cast_sign_loss)]
const fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    if ticks < 0 {
        return None;
    }
    match (ticks as u64).checked_mul(100) {
        Some(nanos) => Some(Duration::from_nanos(nanos)),
        None => None,
    }
}

/// Returns `true` when title or artist differs, indicating a genuine track change.
///
/// `Duration`, `art_url`, `track_number`, and `album` are metadata that can be updated late
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

const fn parse_playback_status(status: WinPlaybackStatus) -> PlaybackState {
    match status {
        WinPlaybackStatus::Playing => PlaybackState::Playing,
        WinPlaybackStatus::Paused => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    /// Mock media session provider for testing.
    struct MockMediaSessionProvider {
        sessions: HashMap<String, PlayerState>,
    }

    impl MockMediaSessionProvider {
        fn new() -> Self {
            Self {
                sessions: HashMap::new(),
            }
        }

        fn with_session(mut self, session_id: &str, state: PlayerState) -> Self {
            self.sessions.insert(session_id.to_string(), state);
            self
        }
    }

    impl MediaSessionProvider for MockMediaSessionProvider {
        async fn get_all_sessions(&self) -> Result<HashMap<String, PlayerState>> {
            Ok(self.sessions.clone())
        }

        async fn get_session_info(&self, session_id: &str) -> Result<PlayerInfo> {
            self.sessions
                .get(session_id)
                .map(|state| PlayerInfo {
                    player_name: session_id.to_string(),
                    current_track: Some(state.track.clone()),
                    playback_state: state.playback_state,
                    position: state.position,
                    volume: state.volume,
                })
                .ok_or_else(|| MediaSourceError::PlayerNotFound(session_id.to_string()))
        }

        fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
            stream::empty()
        }
    }

    fn create_test_track_for_windows(title: &str) -> Track {
        Track {
            title: title.to_string(),
            artist: vec!["Test Artist".to_string()],
            album: Some("Test Album".to_string()),
            album_artist: vec![],
            track_number: None,
            duration: Some(Duration::from_secs(180)),
            art_url: None,
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

    // WindowsMediaSource tests with mock provider

    #[tokio::test]
    async fn test_list_players_with_no_sessions() -> Result<()> {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let source = WindowsMediaSource::with_provider(provider);

        let players = source.list_players().await?;

        assert_eq!(players, Vec::<String>::new());

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_single_session() -> Result<()> {
        let track = create_test_track_for_windows("Test Song");
        let state = create_test_state_for_windows(
            track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let players = source.list_players().await?;

        assert_eq!(players, vec!["Spotify.exe".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_list_players_with_multiple_sessions() -> Result<()> {
        let spotify_track = create_test_track_for_windows("Spotify Song");
        let spotify_state = create_test_state_for_windows(
            spotify_track,
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let vlc_track = create_test_track_for_windows("VLC Song");
        let vlc_state = create_test_state_for_windows(
            vlc_track,
            PlaybackState::Paused,
            Some(Duration::from_secs(30)),
        );
        let provider = Arc::new(
            MockMediaSessionProvider::new()
                .with_session("Spotify.exe", spotify_state)
                .with_session("vlc.exe", vlc_state),
        );
        let source = WindowsMediaSource::with_provider(provider);

        let mut players = source.list_players().await?;
        players.sort();

        assert_eq!(
            players,
            vec!["Spotify.exe".to_string(), "vlc.exe".to_string()]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_with_active_session() -> Result<()> {
        let track = create_test_track_for_windows("Test Song");
        let state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Playing,
            Some(Duration::from_secs(10)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("Spotify.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let player_info = source.get_player("Spotify.exe").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "Spotify.exe".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Playing,
                position: Some(Duration::from_secs(10)),
                volume: None,
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_player_not_found() {
        let provider = Arc::new(MockMediaSessionProvider::new());
        let source = WindowsMediaSource::with_provider(provider);

        let result = source.get_player("nonexistent.exe").await;
        assert_eq!(
            result,
            Err(MediaSourceError::PlayerNotFound(
                "nonexistent.exe".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn test_get_player_paused_state() -> Result<()> {
        let track = create_test_track_for_windows("Paused Song");
        let state = create_test_state_for_windows(
            track.clone(),
            PlaybackState::Paused,
            Some(Duration::from_secs(45)),
        );
        let provider = Arc::new(MockMediaSessionProvider::new().with_session("vlc.exe", state));
        let source = WindowsMediaSource::with_provider(provider);

        let player_info = source.get_player("vlc.exe").await?;

        assert_eq!(
            player_info,
            PlayerInfo {
                player_name: "vlc.exe".to_string(),
                current_track: Some(track),
                playback_state: PlaybackState::Paused,
                position: Some(Duration::from_secs(45)),
                volume: None,
            }
        );

        Ok(())
    }

    // Playback status parsing tests

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

    // ticks_to_duration tests

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
        updated.duration = Some(Duration::from_secs(300)); // duration changed
        updated.art_url = Some("http://example.com/art.jpg".to_string());
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
        let last = Some(Duration::from_secs(60));
        assert!(is_seek(last, Duration::from_secs(10)));
    }

    #[test]
    fn test_is_seek_same_position_not_reported() {
        let last = Some(Duration::from_secs(30));
        assert!(!is_seek(last, Duration::from_secs(30)));
    }
}
