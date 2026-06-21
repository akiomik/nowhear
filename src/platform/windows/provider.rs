//! Windows Media Control session provider and the event-driven monitor.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use futures::future;
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::UnboundedReceiverStream;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as WinSession,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
};
use windows::core::{Result as WindowsResult, RuntimeType};

use super::playback_status::parse_playback_status;
use super::track::{build_track, ticks_to_duration};
use crate::error::{MediaSourceError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};

/// Upper bound on how long a single `get_session_state` read may take.
///
/// The monitor loop reads state to discover and refresh sessions, so a player whose WinRT async
/// call never resolves would otherwise block all further session updates. Bounding the read caps
/// that stall and lets the other sessions proceed.
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

/// Per-session WinRT event-handler registration tokens for event-driven monitoring.
///
/// The Drop impl deregisters all registered event handlers so the Windows OS stops firing
/// callbacks once the session is no longer tracked. Tokens are `Option<i64>`: `None` means the
/// handler was never registered (e.g., due to a transient API error); Drop skips deregistration
/// in that case.
///
/// Unlike the previous design, this holds no `removed` flag: callbacks no longer touch session
/// state directly. Each callback only enqueues a [`RawNotification`], and the single monitor loop
/// ignores notifications whose session id is no longer in its map — so a callback that fires after
/// removal (but before deregistration completes) is harmless without any cross-thread flag.
struct SessionEntry {
    session: WinSession,
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
            track: build_track(&media_props, duration),
            playback_state: parse_playback_status(playback_status),
            position,
            volume: None,
        })
    }

    /// [`get_session_state`](Self::get_session_state) bounded by [`SESSION_STATE_TIMEOUT`].
    ///
    /// A hung player whose WinRT async read never resolves would otherwise stall the monitor
    /// loop indefinitely; on timeout this returns an error so the caller skips the session and
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

    fn create_event_stream(&self) -> impl Stream<Item = MediaEvent> + Send + 'static {
        let manager = self.manager.clone();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        // Raw WinRT notifications funnel through this channel into the single monitor loop.
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            // Register SessionsChanged BEFORE the initial scan so any session change during the
            // scan window is queued as a RawNotification and processed (in order) by the loop
            // afterwards. The callback is now trivial: it does no I/O and touches no shared state,
            // it only enqueues. `UnboundedSender::send` is synchronous, lock-free, and needs no
            // tokio runtime context, so this is safe to call from the arbitrary COM thread WinRT
            // invokes the callback on.
            let raw_tx_manager = raw_tx.clone();
            let sessions_changed_guard = manager
                .SessionsChanged(&TypedEventHandler::new(move |_sender, _args| {
                    let _ = raw_tx_manager.send(RawNotification::SessionsChanged);
                    Ok(())
                }))
                .ok()
                .map(|token| SessionsChangedGuard {
                    manager: manager.clone(),
                    token,
                });

            // The monitor owns all mutable state (the session map and the per-session state
            // cache) and is touched only by this one task. That single-owner design is what lets
            // the previous Arc<Mutex>, two AtomicBools, and the lock-before-snapshot ordering dance
            // disappear: event ordering (e.g. PlayerAdded strictly before the first TrackChanged)
            // now falls out of the loop processing notifications sequentially.
            let mut monitor = EventDrivenPlayerMonitor::new(manager, raw_tx, event_tx);
            monitor.run(raw_rx).await;

            // Loop has exited (consumer dropped the stream). Deregister SessionsChanged by
            // dropping the guard; dropping `monitor` deregisters every per-session handler via
            // SessionEntry::drop. The guard also deregisters during unwinding if this task panics,
            // so a callback can never fire into an orphaned closure.
            drop(sessions_changed_guard);
        });

        UnboundedReceiverStream::new(event_rx)
    }
}

/// A raw notification funnelled from a WinRT callback into the single monitor loop.
///
/// Callbacks never read session state or emit `MediaEvent`s themselves; they only enqueue one of
/// these. The loop does the WinRT reads and the diffing, so all mutable state lives in one place.
enum RawNotification {
    /// The manager's session list changed; the loop should re-enumerate.
    SessionsChanged,
    /// A tracked session signalled a property change (track, playback, or timeline — they are not
    /// distinguished). The loop re-reads that session's full state and diffs it against the cache.
    SessionChanged { id: String },
}

/// Event-driven player monitor using Windows Runtime event handlers.
///
/// This struct is owned and mutated by exactly one task (the loop in [`Self::run`]). WinRT
/// callbacks communicate with it only by sending [`RawNotification`]s, never by touching its
/// fields. That single-owner invariant is what removes the need for any locking or atomics.
struct EventDrivenPlayerMonitor {
    manager: SessionManager,
    /// Handed to each per-session callback so it can enqueue `SessionChanged`.
    raw_tx: mpsc::UnboundedSender<RawNotification>,
    event_tx: mpsc::UnboundedSender<MediaEvent>,
    /// Live sessions and their handler-registration tokens (Drop deregisters).
    sessions: HashMap<String, SessionEntry>,
    /// Last-emitted state per session, used to diff incoming reads. Plain owned values — no lock,
    /// because only this task ever touches the map.
    cache: HashMap<String, PlayerState>,
}

impl EventDrivenPlayerMonitor {
    fn new(
        manager: SessionManager,
        raw_tx: mpsc::UnboundedSender<RawNotification>,
        event_tx: mpsc::UnboundedSender<MediaEvent>,
    ) -> Self {
        Self {
            manager,
            raw_tx,
            event_tx,
            sessions: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Runs the monitor loop until the consumer drops the event stream.
    ///
    /// All notifications are processed strictly sequentially here. This is the heart of the
    /// design: because there is a single processing point, event ordering is automatic
    /// (`PlayerAdded` is emitted before the first `SessionChanged` for that id can be processed,
    /// since the latter sits behind it in the queue) and no field needs synchronisation.
    async fn run(&mut self, mut raw_rx: mpsc::UnboundedReceiver<RawNotification>) {
        // The consumer-dropped signal: `closed()` resolves when the receiver end of `event_tx`
        // (the stream handed to the caller) is dropped, even if no further notification arrives.
        let event_tx = self.event_tx.clone();

        // Initial session scan — retry up to 3 times on transient enumeration failure. Without
        // retry, a player already running at startup is invisible until the next SessionsChanged.
        // TODO: if all 3 attempts fail and no SessionsChanged fires afterward (only one player
        // running and never changing), already-running sessions stay invisible for the stream's
        // lifetime. A long-interval polling fallback would close this gap but reintroduces the
        // polling complexity the event-driven design exists to avoid.
        for attempt in 0..3u32 {
            if self.handle_sessions_changed().await {
                break;
            }
            if attempt < 2 {
                sleep(Duration::from_millis(500)).await;
            }
        }

        loop {
            tokio::select! {
                maybe = raw_rx.recv() => match maybe {
                    Some(RawNotification::SessionsChanged) => {
                        self.handle_sessions_changed().await;
                    }
                    Some(RawNotification::SessionChanged { id }) => {
                        self.handle_session_changed(&id).await;
                    }
                    None => break,
                },
                () = event_tx.closed() => break,
            }
        }
    }

    /// Re-enumerates the manager's sessions, reconciling them against the tracked set.
    ///
    /// Returns `true` if enumeration succeeded (used by the initial-scan retry), `false` on a
    /// transient failure that a later notification will retry.
    async fn handle_sessions_changed(&mut self) -> bool {
        let Ok(sessions_with_ids) =
            WindowsMediaControlProvider::get_sessions_from_manager(&self.manager)
        else {
            return false;
        };

        let current_ids: HashSet<&str> = sessions_with_ids.iter().map(|s| s.id.as_str()).collect();

        // Remove sessions that disappeared. SessionEntry::drop deregisters their handlers.
        let removed_ids: Vec<String> = self
            .sessions
            .keys()
            .filter(|id| !current_ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in removed_ids {
            self.sessions.remove(&id);
            self.cache.remove(&id);
            let _ = self
                .event_tx
                .send(MediaEvent::PlayerRemoved { player_name: id });
        }

        // Register handlers for newly-discovered sessions up front. Registration is synchronous
        // and does no I/O, so a handler starts observing immediately; the authoritative state is
        // read once afterwards (below). Reading after registration means no change can slip
        // through an unwatched window — any change in between simply queues a SessionChanged that
        // the loop processes after this scan, re-reading idempotently.
        let mut pending: Vec<(String, WinSession)> = Vec::new();
        for SessionWithId { id, session } in sessions_with_ids {
            if self.sessions.contains_key(&id) {
                continue;
            }
            let entry = self.register_session_handlers(session.clone(), &id);
            // Keep only sessions whose three handlers all registered. A partial registration is
            // dropped here (deregistering any successful handlers), so a later scan retries.
            if entry.is_fully_registered() {
                self.sessions.insert(id.clone(), entry);
                pending.push((id, session));
            }
        }

        // Read every new session's initial state in parallel. This is the one place reads are
        // batched: a fresh SessionsChanged can surface many players at once, and a slow or hung
        // one must not delay discovering the others. (Steady-state single-session reads in
        // `handle_session_changed` are not batched — see the trade-off note there.)
        let states =
            future::join_all(pending.iter().map(|(_, session)| {
                WindowsMediaControlProvider::get_session_state_bounded(session)
            }))
            .await;

        for ((id, _), state_result) in pending.into_iter().zip(states) {
            let Ok(state) = state_result else {
                // Read failed or timed out: stop tracking (Drop deregisters handlers) so a later
                // SessionsChanged retries.
                // FIXME: if no SessionsChanged fires after this (player already running and
                // stable), this session stays untracked for the stream's lifetime. A per-session
                // background retry would fix it but adds significant complexity.
                self.sessions.remove(&id);
                continue;
            };

            // Seed the cache before emitting so the next SessionChanged diffs against this state.
            self.cache.insert(id.clone(), state.clone());

            // Initial events for a freshly-discovered session: always PlayerAdded, then the
            // current track and state. Emitted here (not via the differ) because discovery always
            // reports the baseline regardless of prior cache contents.
            let _ = self.event_tx.send(MediaEvent::PlayerAdded {
                player_name: id.clone(),
            });
            let _ = self.event_tx.send(MediaEvent::TrackChanged {
                player_name: id.clone(),
                track: state.track,
            });
            let _ = self.event_tx.send(MediaEvent::StateChanged {
                player_name: id,
                state: state.playback_state,
            });
        }

        true
    }

    /// Re-reads one session's full state and emits whatever changed relative to the cache.
    ///
    /// # Trade-off (single-owner loop vs. read latency)
    ///
    /// This read runs inline in the monitor loop, so a player whose WinRT read is slow stalls
    /// processing of other sessions' notifications for up to [`SESSION_STATE_TIMEOUT`]. That bound
    /// is the deliberate cap. We accept it because:
    /// - media notifications are low-frequency, so serial reads rarely queue up;
    /// - keeping the read in the loop is exactly what lets the monitor own its state without locks.
    ///
    /// If steady-state read latency ever becomes a problem, the escape hatch is to spawn the read
    /// as a task and feed its result back as a third `RawNotification` variant (e.g.
    /// `StateRead { id, state }`), turning this into a fully non-blocking actor. That was left out
    /// on purpose: it adds a message type and reorders nothing observable today.
    async fn handle_session_changed(&mut self, id: &str) {
        // Ignore notifications for sessions we no longer track (removed, or registration failed).
        // This is why no `removed` flag is needed: a late callback just looks up a missing id.
        let Some(entry) = self.sessions.get(id) else {
            return;
        };

        let Ok(state) =
            WindowsMediaControlProvider::get_session_state_bounded(&entry.session).await
        else {
            // Read failed or timed out: skip; a later notification retries.
            return;
        };

        let events = diff_player_state(id, self.cache.get(id), &state);
        // Always refresh the cache, even when nothing is emitted, so late-loading metadata
        // (duration, art_url) is retained without surfacing a spurious TrackChanged.
        self.cache.insert(id.to_string(), state);

        for event in events {
            if self.event_tx.send(event).is_err() {
                return;
            }
        }
    }

    /// Registers all three per-session WinRT change handlers.
    ///
    /// Every handler body is now identical and trivial: enqueue `SessionChanged { id }` and
    /// return. They do no I/O, hold no locks, and capture no session state — the loop does the
    /// reading and diffing. The three WinRT events differ only in name, so a tiny helper builds
    /// each registration.
    fn register_session_handlers(&self, session: WinSession, id: &str) -> SessionEntry {
        let token_media_props = Self::register_changed(id, &self.raw_tx, |handler| {
            session.MediaPropertiesChanged(handler)
        });
        let token_playback_info = Self::register_changed(id, &self.raw_tx, |handler| {
            session.PlaybackInfoChanged(handler)
        });
        let token_timeline = Self::register_changed(id, &self.raw_tx, |handler| {
            session.TimelinePropertiesChanged(handler)
        });

        SessionEntry {
            session,
            token_media_props,
            token_playback_info,
            token_timeline,
        }
    }

    /// Builds a `SessionChanged`-enqueuing handler and registers it via `register`.
    ///
    /// Generic over the WinRT event-args type `A` so the same trivial closure serves all three
    /// change events. Returns the registration token, or `None` on a transient registration error.
    fn register_changed<A: RuntimeType + 'static>(
        id: &str,
        raw_tx: &mpsc::UnboundedSender<RawNotification>,
        register: impl FnOnce(&TypedEventHandler<WinSession, A>) -> WindowsResult<i64>,
    ) -> Option<i64> {
        let raw_tx = raw_tx.clone();
        let id = id.to_string();
        register(&TypedEventHandler::new(move |_sender, _args| {
            let _ = raw_tx.send(RawNotification::SessionChanged { id: id.clone() });
            Ok(())
        }))
        .ok()
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

/// Diffs a freshly-read [`PlayerState`] against the cached one and returns the events to emit.
///
/// This is the single, platform-agnostic place where Windows' "something changed, re-read
/// everything" notifications are turned into discrete `MediaEvent`s. Collapsing the three former
/// per-handler diff paths into one pure function is what let the three WinRT handlers become
/// identical. Events are ordered `TrackChanged`, `StateChanged`, `PositionChanged`.
///
/// `old` is `None` only when no prior state is cached (the differ is not used for the very first
/// emission — see `handle_sessions_changed`, which emits the baseline directly).
fn diff_player_state(
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

    events
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(Duration::from_secs(120)),
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
        new.track.art_url = Some("http://example.com/a.jpg".to_string());
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
        new.position = Some(Duration::from_secs(60));
        let events = diff_player_state("p", Some(&old), &new);
        assert_eq!(
            events,
            vec![MediaEvent::PositionChanged {
                player_name: "p".to_string(),
                position: Duration::from_secs(60),
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
}
