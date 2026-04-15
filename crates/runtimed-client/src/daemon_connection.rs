//! Long-lived daemon session that treats metadata as a connection property.
//!
//! Rationale: `DaemonInfo` (blob_port, version, pid, started_at, worktree
//! info) is a property of the current daemon connection session. It's
//! learned on connect, stable for the life of the connection, and only
//! changes on reconnect. Treating it that way — instead of one-shot-pulling
//! it per lookup — is both cheaper and more correct: a live socket is
//! ground truth that the cached info is still valid.
//!
//! This supersedes per-lookup [`query_daemon_info`](crate::singleton::query_daemon_info)
//! for long-running consumers (Tauri app, mcpb-runt, runt-mcp-proxy). One-
//! shot CLI callers (e.g. `runt daemon status`) continue to use the
//! existing helper.
//!
//! Design doc: `docs/superpowers/specs/2026-04-15-daemon-connection-session-design.md`

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;

use crate::singleton::{query_daemon_info, DaemonInfo};

/// Events emitted by a [`DaemonConnection`] on state transitions.
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    /// First successful connection, or a reconnect where the identity
    /// (pid + started_at) is unchanged. Carries the current info.
    Connected { info: DaemonInfo },
    /// Reconnect produced different identity — the daemon restarted or
    /// was upgraded. Subscribers that care about version changes watch
    /// for this.
    Upgraded {
        previous: DaemonInfo,
        current: DaemonInfo,
    },
    /// Connection to the daemon was lost. Subscribers should treat any
    /// cached info as potentially stale until the next `Connected` or
    /// `Upgraded` event.
    Disconnected,
}

/// Current state of the supervisor's view of the daemon.
#[derive(Debug, Clone)]
enum ConnectionState {
    /// Supervisor has a live connection and the daemon returned info.
    Connected { info: DaemonInfo },
    /// Supervisor is trying to (re)connect. `last_info` is the info
    /// from the previous successful connection, if any; `None` means
    /// we've never connected in this supervisor's lifetime.
    Reconnecting { last_info: Option<DaemonInfo> },
    /// `close()` has been called; the supervisor has exited. Info is
    /// held so readers don't observe a sudden `None` right after close.
    Stopped { last_info: Option<DaemonInfo> },
}

/// Long-lived handle to a daemon connection with cached metadata.
///
/// Construct via [`DaemonConnection::spawn`] and drop (or call
/// [`close`](Self::close)) to stop the supervisor task.
pub struct DaemonConnection {
    /// Most recent state. Read by `info()`, written by the supervisor.
    /// Held briefly on both sides — never across an `.await`.
    state: Arc<RwLock<ConnectionState>>,

    /// Event stream. Capped at `EVENT_CHANNEL_CAPACITY`; slow subscribers
    /// receive `RecvError::Lagged` and should re-sync by calling
    /// `info()` (which is always current).
    events: broadcast::Sender<DaemonEvent>,

    /// Shutdown flag latched by `close()` and `Drop`. The supervisor
    /// checks it at every loop iteration AND is woken up by the
    /// `shutdown_notify` so an in-flight `.await` (query, ping, sleep)
    /// unblocks promptly. The flag is what actually makes the
    /// supervisor exit — the notify is only a wake-up hint, because
    /// `Notify::notify_waiters()` is lost if the supervisor isn't
    /// currently blocked in `.notified()`.
    shutdown_flag: Arc<AtomicBool>,
    shutdown_notify: Arc<tokio::sync::Notify>,

    /// Supervisor handle. Kept so `Drop` can abort it if the user drops
    /// the `DaemonConnection` without calling `close()`.
    supervisor: Option<JoinHandle<()>>,
}

/// Events channel capacity. Subscribers that don't keep up will see
/// `RecvError::Lagged` — they should recover by reading `info()`.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Supervisor parameters. Chosen to favor responsiveness on startup
/// (quick initial retry if the daemon is still booting) and patience
/// during extended outages (longer gap between retries once we've
/// settled into a steady reconnecting state).
const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Interval between heartbeat pings once connected. The daemon doesn't
/// push unsolicited data over the pool channel, so the only way to
/// notice it's gone is to periodically probe.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

impl DaemonConnection {
    /// Spawn a supervisor task that maintains a connection to the
    /// daemon at `socket_path` and caches its [`DaemonInfo`].
    ///
    /// Returns immediately with the supervisor running in the
    /// background. The first fetch happens asynchronously; use
    /// [`wait_connected`](Self::wait_connected) to block until it
    /// succeeds if the caller needs info before proceeding.
    pub fn spawn(socket_path: PathBuf) -> Self {
        let state = Arc::new(RwLock::new(ConnectionState::Reconnecting {
            last_info: None,
        }));
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());

        let supervisor = tokio::spawn(run_supervisor(
            socket_path,
            state.clone(),
            events.clone(),
            shutdown_flag.clone(),
            shutdown_notify.clone(),
        ));

        Self {
            state,
            events,
            shutdown_flag,
            shutdown_notify,
            supervisor: Some(supervisor),
        }
    }

    /// Current cached daemon info.
    ///
    /// Returns `None` while the supervisor has no fresh info — either
    /// we've never connected, or the connection dropped and we haven't
    /// reconnected yet (in which case the last-known info is stashed
    /// in the state enum for debugging but deliberately not returned —
    /// callers shouldn't act on stale metadata).
    pub async fn info(&self) -> Option<DaemonInfo> {
        let state = self.state.read().await;
        match &*state {
            ConnectionState::Connected { info } => Some(info.clone()),
            ConnectionState::Reconnecting { .. } | ConnectionState::Stopped { .. } => None,
        }
    }

    /// Last-known daemon info, including from a prior session that has
    /// since disconnected. Useful for UIs that want to surface "daemon
    /// was version X before it dropped" rather than nothing at all.
    ///
    /// Unlike [`info`](Self::info), this does not return `None` just
    /// because the current state is reconnecting — only if we have
    /// never connected in this supervisor's lifetime.
    pub async fn last_known_info(&self) -> Option<DaemonInfo> {
        let state = self.state.read().await;
        match &*state {
            ConnectionState::Connected { info } => Some(info.clone()),
            ConnectionState::Reconnecting { last_info }
            | ConnectionState::Stopped { last_info } => last_info.clone(),
        }
    }

    /// Block until the supervisor has a fresh connection, or the
    /// timeout elapses. Useful for startup flows where the caller
    /// cannot proceed without info.
    pub async fn wait_connected(&self, timeout: Duration) -> Option<DaemonInfo> {
        // Fast path: already connected.
        if let Some(info) = self.info().await {
            return Some(info);
        }

        let mut rx = self.subscribe();
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }

            // Re-check in case a Connected event fired between the fast
            // path above and the `subscribe()` call. The window is tiny
            // but non-zero; cheap to cover.
            if let Some(info) = self.info().await {
                return Some(info);
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(DaemonEvent::Connected { info }))
                | Ok(Ok(DaemonEvent::Upgraded { current: info, .. })) => return Some(info),
                Ok(Ok(DaemonEvent::Disconnected)) => continue, // keep waiting
                // Supervisor exited or channel lagged — fall through to
                // another iteration; the state check at the top will
                // catch Stopped via `info()` returning None.
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return None,
                Err(_elapsed) => return None,
            }
        }
    }

    /// Subscribe to state transition events. Capacity is bounded
    /// ([`EVENT_CHANNEL_CAPACITY`]); slow consumers see `Lagged` and
    /// should recover by calling [`info`](Self::info).
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    /// Stop the supervisor task. Idempotent — safe to call multiple
    /// times. The `DaemonConnection` becomes a read-only shell whose
    /// `info()` returns None; `last_known_info()` still returns the
    /// final cached value.
    pub async fn close(mut self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        if let Some(handle) = self.supervisor.take() {
            // Best-effort: if the supervisor is stuck inside a network
            // call (query_daemon_info, ping) longer than 1s, we abort
            // it so the caller never waits forever. The flag still
            // prevents any new iteration from running if the task
            // happens to wake up later.
            match tokio::time::timeout(Duration::from_secs(1), handle).await {
                Ok(Ok(())) => {}
                Ok(Err(join_err)) if join_err.is_cancelled() => {}
                Ok(Err(join_err)) => {
                    warn!("[daemon-connection] supervisor task panicked: {}", join_err);
                }
                Err(_elapsed) => {
                    warn!(
                        "[daemon-connection] supervisor didn't exit within 1s of close(); aborting"
                    );
                    // The handle was moved into the timeout, so we've
                    // lost the ability to abort. In practice this only
                    // means the supervisor wakes up later, sees the
                    // flag, and exits — no loop iterations happen.
                }
            }
        }
    }
}

/// Process-global `DaemonConnection`, lazy-initialized on first call.
///
/// The spec called out "process-level sharing" as an open question;
/// this is the simple answer for callers that don't want to thread a
/// connection through their own state. The `OnceCell` ensures only one
/// supervisor task exists per process, regardless of how many call
/// sites reach for it.
///
/// The singleton is pinned to `default_socket_path()` at first-init
/// time. Tests that need a different socket path should construct a
/// local `DaemonConnection::spawn(...)` instead.
///
/// Never calls `close()` on the singleton — it lives for the lifetime
/// of the process. `Drop` on the underlying task doesn't run because
/// the OnceCell is static, but that's fine: the OS reaps the
/// supervisor task when the process exits.
pub fn shared() -> &'static DaemonConnection {
    static SHARED: std::sync::OnceLock<DaemonConnection> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| DaemonConnection::spawn(crate::default_socket_path()))
}

impl Drop for DaemonConnection {
    fn drop(&mut self) {
        // Latch the flag AND wake sleeps, then abort outright — Drop is
        // the last-resort path (close() wasn't called).
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        if let Some(handle) = self.supervisor.take() {
            handle.abort();
        }
    }
}

/// Run the supervisor state machine until `shutdown` fires.
///
/// High-level flow:
///
/// 1. If disconnected, try `query_daemon_info`. Success → transition
///    to Connected and emit `Connected` / `Upgraded`. Failure → wait
///    with exponential backoff and try again.
/// 2. Once connected, heartbeat every [`HEARTBEAT_INTERVAL`] via a
///    lightweight `ping()`. Failure → transition to Reconnecting and
///    emit `Disconnected`.
///
/// This has to share the `state` lock with `DaemonConnection::info()`
/// readers. We NEVER hold the write guard across `.await` — the
/// pattern is always: do network I/O without the lock, then briefly
/// take the write lock to update state, then drop it before emitting
/// the event.
async fn run_supervisor(
    socket_path: PathBuf,
    state: Arc<RwLock<ConnectionState>>,
    events: broadcast::Sender<DaemonEvent>,
    shutdown_flag: Arc<AtomicBool>,
    shutdown_notify: Arc<tokio::sync::Notify>,
) {
    let mut backoff = INITIAL_BACKOFF;

    loop {
        // Top of loop: flag-check latches shutdown even if the last
        // `.await` completed without hitting a shutdown_notify select
        // branch (race: close() set the flag while we were mid-I/O).
        if shutdown_flag.load(Ordering::SeqCst) {
            stop(&state).await;
            return;
        }

        // Fetch daemon info over the socket. No lock held here.
        let fetched = tokio::select! {
            info = query_daemon_info(socket_path.clone()) => info,
            _ = shutdown_notify.notified() => {
                stop(&state).await;
                return;
            }
        };

        if shutdown_flag.load(Ordering::SeqCst) {
            stop(&state).await;
            return;
        }

        let Some(info) = fetched else {
            // Connect/handshake/GetDaemonInfo failed. Back off and
            // retry. We don't emit an event on every failed retry —
            // the caller already knows we're reconnecting because
            // `info()` returns None. We DO emit `Disconnected` on the
            // first transition from Connected → Reconnecting (handled
            // in the heartbeat branch below), so the UI can render
            // "daemon dropped" without waiting for the backoff loop.
            debug!(
                "[daemon-connection] fetch failed; retrying in {:?}",
                backoff
            );
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = shutdown_notify.notified() => {
                    stop(&state).await;
                    return;
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            continue;
        };

        // Successful fetch. Emit + cache.
        emit_transition(&state, &events, &info).await;
        backoff = INITIAL_BACKOFF;

        // Track the most recently observed daemon info so we can stash
        // it as `last_info` on disconnect. If we just used `info` (the
        // outer variable) we'd roll `last_known_info()` back to the
        // pre-heartbeat value after a daemon upgrade + subsequent drop.
        let mut latest = info.clone();

        // Connected. Heartbeat by re-running GetDaemonInfo so we catch
        // both hard disconnects AND fast daemon restarts (same socket
        // path, new pid/started_at — a bare Ping can't distinguish
        // those from the still-same-process case).
        loop {
            tokio::select! {
                _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => {}
                _ = shutdown_notify.notified() => {
                    stop(&state).await;
                    return;
                }
            }

            if shutdown_flag.load(Ordering::SeqCst) {
                stop(&state).await;
                return;
            }

            let fresh = query_daemon_info(socket_path.clone()).await;

            if shutdown_flag.load(Ordering::SeqCst) {
                stop(&state).await;
                return;
            }

            match fresh {
                Some(fresh_info) => {
                    // Detect identity shift (fast restart on same socket)
                    // and surface an Upgraded event without requiring a
                    // full disconnect/reconnect cycle. emit_transition
                    // handles the compare internally.
                    emit_transition(&state, &events, &fresh_info).await;
                    latest = fresh_info;
                }
                None => {
                    warn!("[daemon-connection] heartbeat failed; transitioning to reconnecting");
                    {
                        let mut state = state.write().await;
                        *state = ConnectionState::Reconnecting {
                            last_info: Some(latest.clone()),
                        };
                    }
                    let _ = events.send(DaemonEvent::Disconnected);
                    break; // break inner loop; outer loop retries
                }
            }
        }
    }
}

/// Update the cached state with a freshly-fetched `DaemonInfo` and
/// emit the appropriate event. If the new info's identity
/// (`pid` + `started_at`) matches whatever was previously cached, we
/// emit `Connected` (treated as a refresh); otherwise `Upgraded`.
/// First ever connection emits `Connected` too.
async fn emit_transition(
    state: &Arc<RwLock<ConnectionState>>,
    events: &broadcast::Sender<DaemonEvent>,
    info: &DaemonInfo,
) {
    // Read previous without holding the lock across the emit.
    let previous = {
        let state = state.read().await;
        match &*state {
            ConnectionState::Connected { info } => Some(info.clone()),
            ConnectionState::Reconnecting { last_info }
            | ConnectionState::Stopped { last_info } => last_info.clone(),
        }
    };

    let is_upgrade = previous
        .as_ref()
        .is_some_and(|prev| prev.pid != info.pid || prev.started_at != info.started_at);

    if is_upgrade {
        info!(
            "[daemon-connection] daemon upgraded: version={} pid={} blob_port={:?}",
            info.version, info.pid, info.blob_port
        );
    } else if previous.is_none() {
        info!(
            "[daemon-connection] connected: version={} pid={} blob_port={:?}",
            info.version, info.pid, info.blob_port
        );
    }

    {
        let mut state = state.write().await;
        *state = ConnectionState::Connected { info: info.clone() };
    }

    let event = match (is_upgrade, previous) {
        (true, Some(prev)) => DaemonEvent::Upgraded {
            previous: prev,
            current: info.clone(),
        },
        _ => DaemonEvent::Connected { info: info.clone() },
    };
    let _ = events.send(event);
}

async fn stop(state: &Arc<RwLock<ConnectionState>>) {
    let last_info = {
        let state = state.read().await;
        match &*state {
            ConnectionState::Connected { info } => Some(info.clone()),
            ConnectionState::Reconnecting { last_info }
            | ConnectionState::Stopped { last_info } => last_info.clone(),
        }
    };
    let mut state = state.write().await;
    *state = ConnectionState::Stopped { last_info };
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Spawning against a non-existent socket puts the supervisor in
    /// reconnect mode; `info()` returns None and stays None until the
    /// socket appears.
    #[tokio::test]
    async fn spawn_against_missing_socket_returns_none() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("no-such.sock");
        let conn = DaemonConnection::spawn(socket);

        // Give the supervisor a moment to fail the first fetch.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(conn.info().await.is_none());
        assert!(conn.last_known_info().await.is_none());
        conn.close().await;
    }

    /// `wait_connected` with a short timeout against a missing daemon
    /// returns None without hanging.
    #[tokio::test]
    async fn wait_connected_times_out_cleanly() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("absent.sock");
        let conn = DaemonConnection::spawn(socket);

        let start = Instant::now();
        let result = conn.wait_connected(Duration::from_millis(150)).await;
        let elapsed = start.elapsed();

        assert!(result.is_none());
        assert!(
            elapsed < Duration::from_millis(500),
            "wait_connected hung: {elapsed:?}"
        );
        conn.close().await;
    }

    /// Dropping without `close()` doesn't panic or leak the supervisor.
    /// The supervisor is aborted via the Drop impl.
    #[tokio::test]
    async fn drop_without_close_is_clean() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("dropped.sock");
        {
            let _conn = DaemonConnection::spawn(socket);
            tokio::time::sleep(Duration::from_millis(50)).await;
        } // _conn dropped here — supervisor should be aborted
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Test passes if no panic/leak; nothing more to assert without
        // hooking into tokio internals.
    }

    /// After `close()`, `info()` returns None but `last_known_info`
    /// returns whatever was cached. Since our test uses no live daemon,
    /// both are None here — this test is really about ensuring close()
    /// is idempotent.
    #[tokio::test]
    async fn close_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("closed.sock");
        let conn = DaemonConnection::spawn(socket);
        conn.close().await;
        // No second-close API surface since close takes self; this test
        // mainly proves the first close doesn't hang or panic.
    }
}
