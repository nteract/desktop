//! Daemon watch loop driven by `DaemonConnection` events.
//!
//! Replaces the old `health.rs` ping-and-backoff loop. `DaemonConnection`
//! (in `runtimed-client`) already maintains a long-lived supervisor that
//! caches `DaemonInfo` and emits `Connected`/`Upgraded`/`Disconnected`.
//! This module consumes that stream and performs the two actions that are
//! specific to the MCP server:
//!
//! 1. Exit the process on a version change so the proxy respawns us with
//!    the new binary.
//! 2. Re-join the active notebook session when the daemon comes back
//!    (either after a brief disconnect, or after a same-version restart).
//!
//! Tool dispatch is no longer gated on a locally-tracked state — under
//! sustained concurrent load the old loop could stall in `Reconnecting`
//! while the daemon was actually healthy, short-circuiting every tool
//! call. See #2000.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtimed_client::client::PoolClient;
use runtimed_client::daemon_connection::{DaemonConnection, DaemonEvent};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

use crate::session::NotebookSession;

/// Exit code when the daemon has been upgraded and the MCP server should
/// restart. EX_TEMPFAIL (sysexits.h) — "temporary failure; try again."
pub const EXIT_DAEMON_UPGRADED: i32 = 75;

/// Env var the proxy sets on the restarted child to hand off the notebook
/// the previous child was attached to. Value is either a UUID or an
/// absolute file path.
pub const REJOIN_ENV_VAR: &str = "NTERACT_MCP_REJOIN_NOTEBOOK";

const REJOIN_RETRY_DELAY: Duration = Duration::from_secs(1);
const REJOIN_MAX_RETRIES: u32 = 3;

/// What the watch loop should do in response to a `DaemonEvent`.
#[derive(Debug, PartialEq, Eq)]
enum WatchDecision {
    /// Exit the process with the given code (daemon upgraded).
    Exit(i32),
    /// Rejoin using the provided initial target (UUID or file path) from
    /// `NTERACT_MCP_REJOIN_NOTEBOOK` — for the restarted-child case.
    RejoinInitial(String),
    /// Rejoin using the current session's state — for reconnect or
    /// same-version restart while we already have a session.
    RejoinContinuation,
    /// Record that the daemon was lost. The watch loop uses this to
    /// gate `RejoinContinuation` — only after a disconnect.
    MarkDisconnected,
    /// Nothing to do.
    NoOp,
}

/// Classify a `DaemonEvent` into the action the watch loop should take.
///
/// `initial_target` is **not consumed** by `classify()`. The watch loop
/// is responsible for clearing it after a successful rejoin. This ensures
/// the target survives failed rejoin attempts and can be retried on the
/// next `Connected` event.
///
/// `was_disconnected` tracks whether the daemon connection was lost since
/// the last successful join. This prevents the 10-second heartbeat
/// `Connected` events from triggering spurious rejoins — only a
/// `Connected` event that follows an actual `Disconnected` triggers a
/// `RejoinContinuation`. Without this, every heartbeat creates a brief
/// 2→1 peer cycle that keeps the room alive indefinitely (#2088).
fn classify(
    event: &DaemonEvent,
    initial_target: &Option<String>,
    has_session: bool,
    was_disconnected: bool,
) -> WatchDecision {
    match event {
        DaemonEvent::Upgraded { previous, current } => {
            if previous.version != current.version {
                return WatchDecision::Exit(EXIT_DAEMON_UPGRADED);
            }
            // Same-version restart (new pid) always needs a rejoin —
            // the old peer connection is dead regardless of
            // was_disconnected (the daemon process recycled).
            if let Some(t) = initial_target.as_ref() {
                WatchDecision::RejoinInitial(t.clone())
            } else if has_session {
                WatchDecision::RejoinContinuation
            } else {
                WatchDecision::NoOp
            }
        }
        DaemonEvent::Connected { .. } => {
            // Initial target always takes priority (proxy hand-off).
            if let Some(t) = initial_target.as_ref() {
                return WatchDecision::RejoinInitial(t.clone());
            }
            // Only rejoin after a real disconnect, not on routine
            // heartbeat refreshes. DaemonConnection emits Connected
            // every HEARTBEAT_INTERVAL (10s); without this gate the
            // watch loop would reconnect every 10s, creating a brief
            // 2-peer spike that resets the eviction timer (#2088).
            if has_session && was_disconnected {
                WatchDecision::RejoinContinuation
            } else {
                WatchDecision::NoOp
            }
        }
        DaemonEvent::Disconnected => WatchDecision::MarkDisconnected,
    }
}

/// Run the watch loop to completion. Returns the exit code the caller
/// should use; 0 means the event stream closed cleanly.
pub async fn watch(
    daemon_conn: Arc<DaemonConnection>,
    socket_path: PathBuf,
    session: Arc<RwLock<Option<NotebookSession>>>,
    peer_label: Arc<RwLock<String>>,
) -> i32 {
    let mut rx = daemon_conn.subscribe();
    let mut initial_target: Option<String> = std::env::var(REJOIN_ENV_VAR).ok();
    if initial_target.is_some() {
        info!("Seeded initial rejoin target from {REJOIN_ENV_VAR}");
    }

    // Track whether we've been through a Disconnected state.
    // `initial_target.is_some()` seeds this to true so the first
    // Connected event (which always fires on supervisor startup)
    // triggers the initial rejoin without requiring a prior disconnect.
    let mut was_disconnected = initial_target.is_some();

    loop {
        let event = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Daemon event stream lagged, dropped {n} events");
                // Treat a lag as a potential disconnect — we may have
                // missed a Disconnected event in the dropped batch.
                was_disconnected = true;
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return 0,
        };

        let has_session = session.read().await.is_some();
        match classify(&event, &initial_target, has_session, was_disconnected) {
            WatchDecision::Exit(code) => {
                if let DaemonEvent::Upgraded { previous, current } = &event {
                    info!(
                        "Daemon upgraded ({} → {}), exiting for proxy respawn",
                        previous.version, current.version
                    );
                }
                return code;
            }
            WatchDecision::RejoinInitial(target) => {
                info!("Performing initial rejoin to {target}");
                let ok = rejoin(&socket_path, &session, &peer_label, Some(target)).await;
                // Only clear the disconnect flag and consume the initial
                // target if rejoin succeeded or the session was explicitly
                // cleared (room evicted). If rejoin exhausted retries,
                // keep both was_disconnected=true and initial_target
                // intact so the next Connected event retries.
                if ok {
                    was_disconnected = false;
                    initial_target = None;
                }
            }
            WatchDecision::RejoinContinuation => {
                info!("Daemon reachable, rejoining notebook session");
                let ok = rejoin(&socket_path, &session, &peer_label, None).await;
                if ok {
                    was_disconnected = false;
                }
            }
            WatchDecision::MarkDisconnected => {
                was_disconnected = true;
            }
            WatchDecision::NoOp => {}
        }
    }
}

/// Decide whether a target string should be treated as a notebook UUID
/// or a file path.
fn looks_like_uuid(target: &str) -> bool {
    let path = std::path::Path::new(target);
    path.components().count() == 1
        && path.extension().is_none()
        && uuid::Uuid::parse_str(target).is_ok()
}

/// Re-join the active notebook session.
///
/// If `override_target` is provided, use it instead of whatever session is
/// currently stored — this is how the proxy hands off the previous
/// notebook_id to a freshly respawned child via `NTERACT_MCP_REJOIN_NOTEBOOK`.
///
/// For file-backed notebooks, uses `connect_open(path)` so the daemon
/// reloads from disk (the UUID-only path would yield an empty document
/// because file-backed rooms' `.automerge` persist files are deleted).
///
/// For ephemeral notebooks, checks `list_rooms` first to verify the room
/// still exists in the daemon. If the room was evicted during the
/// disconnect, the session is cleared immediately without creating a new
/// peer connection — avoiding the creation of phantom rooms (#2088).
///
/// Returns `true` if the rejoin succeeded or the session was explicitly
/// cleared (room evicted). Returns `false` if retries were exhausted
/// without success — the caller should keep `was_disconnected` true so
/// the next `Connected` event retries.
async fn rejoin(
    socket_path: &Path,
    session: &Arc<RwLock<Option<NotebookSession>>>,
    peer_label: &Arc<RwLock<String>>,
    override_target: Option<String>,
) -> bool {
    let (notebook_id, notebook_path) = match override_target {
        Some(target) if looks_like_uuid(&target) => (target, None),
        Some(target) => {
            // Treat as file path. We'll learn the real notebook_id from
            // connect_open's response.
            (target.clone(), Some(target))
        }
        None => {
            let guard = session.read().await;
            match guard.as_ref() {
                Some(s) => (s.notebook_id.clone(), s.notebook_path.clone()),
                None => return true, // No session to rejoin — not a failure
            }
        }
    };

    // For ephemeral notebooks (no file path), verify the room still exists
    // in the daemon before attempting to rejoin. This is the explicit signal
    // that the room was evicted — no heuristics needed. Without this check,
    // a `connect(uuid)` to an evicted room would create a new empty room,
    // wasting a kernel and preventing proper eviction (#2088).
    let has_file = notebook_path
        .as_ref()
        .is_some_and(|p| std::path::Path::new(p.as_str()).exists());
    if !has_file {
        let client = PoolClient::new(socket_path.to_path_buf());
        match client.list_rooms().await {
            Ok(rooms) => {
                if !rooms.iter().any(|r| r.notebook_id == notebook_id) {
                    info!(
                        "Room {notebook_id} no longer exists in daemon; \
                         clearing session (notebook was evicted)"
                    );
                    *session.write().await = None;
                    return true; // Session cleared intentionally
                }
            }
            Err(e) => {
                warn!("list_rooms failed during rejoin check: {e}");
                // Can't verify — fall through to the connect attempt which
                // will also fail if the daemon is truly unreachable.
            }
        }
    }

    let label = peer_label.read().await.clone();

    for attempt in 0..=REJOIN_MAX_RETRIES {
        let use_path = notebook_path
            .as_ref()
            .filter(|p| std::path::Path::new(p.as_str()).exists());

        let result = if let Some(path) = use_path {
            match notebook_sync::connect::connect_open(
                socket_path.to_path_buf(),
                PathBuf::from(path),
                &label,
            )
            .await
            {
                Ok(r) => {
                    let handle = r.handle;
                    let broadcast_rx = r.broadcast_rx;
                    if let Err(e) = handle.await_initial_load_ready().await {
                        Err(e)
                    } else {
                        let cell_count = handle.get_cells().len();
                        Ok((handle, broadcast_rx, cell_count, r.info.notebook_id))
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            match notebook_sync::connect::connect(
                socket_path.to_path_buf(),
                notebook_id.clone(),
                &label,
            )
            .await
            {
                Ok(r) => {
                    let handle = r.handle;
                    let broadcast_rx = r.broadcast_rx;
                    if let Err(e) = handle.await_initial_load_ready().await {
                        Err(e)
                    } else {
                        let cell_count = handle.get_cells().len();
                        Ok((handle, broadcast_rx, cell_count, notebook_id.clone()))
                    }
                }
                Err(e) => Err(e),
            }
        };

        match result {
            Ok((handle, broadcast_rx, new_cell_count, new_notebook_id)) => {
                crate::presence::announce(&handle, &label).await;

                let new_session = NotebookSession {
                    handle,
                    broadcast_rx,
                    notebook_id: new_notebook_id,
                    notebook_path: notebook_path.clone(),
                };
                *session.write().await = Some(new_session);
                info!("Rejoined notebook session ({new_cell_count} cells)");
                return true;
            }
            Err(e) => {
                if attempt < REJOIN_MAX_RETRIES {
                    warn!(
                        "Rejoin attempt {} failed (retrying in {}s): {e}",
                        attempt + 1,
                        REJOIN_RETRY_DELAY.as_secs()
                    );
                    tokio::time::sleep(REJOIN_RETRY_DELAY).await;
                } else {
                    warn!("Rejoin exhausted retries: {e}");
                }
            }
        }
    }

    false // All retries exhausted
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use runtimed_client::singleton::DaemonInfo;

    fn info_with(version: &str, pid: u32) -> DaemonInfo {
        DaemonInfo {
            endpoint: "/tmp/test.sock".to_string(),
            pid,
            version: version.to_string(),
            started_at: Utc::now(),
            blob_port: None,
            worktree_path: None,
            workspace_description: None,
        }
    }

    #[test]
    fn version_change_triggers_exit() {
        let event = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.1.0", 200),
        };
        let initial = None;
        // Version change exits regardless of was_disconnected.
        assert_eq!(
            classify(&event, &initial, false, false),
            WatchDecision::Exit(EXIT_DAEMON_UPGRADED)
        );
    }

    #[test]
    fn same_version_restart_triggers_continuation_rejoin() {
        // Upgraded (same-version) always triggers rejoin — the daemon
        // process recycled so the old peer is dead. was_disconnected
        // is irrelevant for Upgraded events.
        let event = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        let initial = None;
        assert_eq!(
            classify(&event, &initial, true, false),
            WatchDecision::RejoinContinuation
        );
    }

    #[test]
    fn same_version_restart_without_session_is_noop() {
        let event = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        let initial = None;
        assert_eq!(
            classify(&event, &initial, false, false),
            WatchDecision::NoOp
        );
    }

    #[test]
    fn connected_returns_initial_target_without_consuming() {
        let event = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        let initial = Some("abc-uuid".to_string());
        // Initial target triggers RejoinInitial but classify() does NOT
        // consume it — the watch loop consumes after successful rejoin.
        assert_eq!(
            classify(&event, &initial, false, false),
            WatchDecision::RejoinInitial("abc-uuid".to_string())
        );
        assert!(
            initial.is_some(),
            "classify must not consume initial target"
        );

        // With initial_target still present, next Connected still returns
        // RejoinInitial (retry semantics — will keep trying until the
        // watch loop clears it after a successful rejoin).
        assert_eq!(
            classify(&event, &initial, false, false),
            WatchDecision::RejoinInitial("abc-uuid".to_string())
        );
    }

    #[test]
    fn cleared_initial_target_falls_through() {
        let event = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        // After the watch loop clears initial_target (on successful rejoin),
        // subsequent Connected events without session/disconnect are NoOp.
        let initial: Option<String> = None;
        assert_eq!(
            classify(&event, &initial, false, false),
            WatchDecision::NoOp
        );
    }

    #[test]
    fn disconnected_marks_disconnected() {
        let initial = Some("abc".to_string());
        assert_eq!(
            classify(&DaemonEvent::Disconnected, &initial, true, false),
            WatchDecision::MarkDisconnected
        );
        assert!(
            initial.is_some(),
            "disconnect must not consume initial target"
        );
    }

    #[test]
    fn uuid_target_detected() {
        assert!(looks_like_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!looks_like_uuid("/tmp/notebook.ipynb"));
        assert!(!looks_like_uuid("notebook.ipynb"));
        assert!(!looks_like_uuid("relative/path"));
    }

    /// Connected events that are just heartbeat refreshes (no prior
    /// disconnect) must NOT trigger RejoinContinuation. This is the
    /// primary fix for #2088 — without this gate, every 10s heartbeat
    /// Connected event would create a brief 2→1 peer cycle that resets
    /// the eviction timer, keeping the room alive indefinitely.
    #[test]
    fn heartbeat_connected_does_not_rejoin() {
        let event = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        let initial = None;

        // has_session=true but was_disconnected=false (steady-state
        // heartbeat) → must be NoOp, not RejoinContinuation.
        assert_eq!(
            classify(&event, &initial, true, false),
            WatchDecision::NoOp,
            "heartbeat Connected must not trigger rejoin"
        );
    }

    /// Connected events AFTER a Disconnected should trigger
    /// RejoinContinuation — the peer connection was actually lost.
    #[test]
    fn reconnect_after_disconnect_triggers_rejoin() {
        let connected = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        let initial = None;

        // After disconnect, Connected should trigger rejoin.
        assert_eq!(
            classify(&connected, &initial, true, true),
            WatchDecision::RejoinContinuation
        );
    }

    /// After an ephemeral notebook is evicted and the session is cleared,
    /// subsequent Connected/Upgraded events should produce NoOp (not
    /// RejoinContinuation). This regression test verifies the fix for #2088
    /// — without clearing the session, the watch loop would reconnect every
    /// 10s, briefly creating peers and preventing proper room eviction.
    #[test]
    fn cleared_session_stops_continuation_rejoins() {
        let event = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        let initial = None;

        // With has_session=true AND was_disconnected=true, we get
        // RejoinContinuation.
        assert_eq!(
            classify(&event, &initial, true, true),
            WatchDecision::RejoinContinuation
        );

        // After the session is cleared (has_session=false), same event
        // is NoOp even with was_disconnected=true.
        assert_eq!(classify(&event, &initial, false, true), WatchDecision::NoOp);

        // Same for Upgraded (same-version restart).
        let upgraded = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        assert_eq!(
            classify(&upgraded, &initial, false, false),
            WatchDecision::NoOp
        );
    }

    /// When rejoin fails (returns false), initial_target must survive for
    /// retry on the next Connected event. This test simulates the classify
    /// behavior: with initial_target present, classify always returns
    /// RejoinInitial — it never consumes the target. The watch loop only
    /// clears it after successful rejoin.
    #[test]
    fn failed_initial_rejoin_preserves_target_for_retry() {
        let connected = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };

        // Simulate the watch loop's initial_target across multiple events.
        let mut initial_target = Some("target-uuid".to_string());

        // First Connected → RejoinInitial.
        assert_eq!(
            classify(&connected, &initial_target, false, true),
            WatchDecision::RejoinInitial("target-uuid".to_string())
        );

        // Simulate rejoin failure (watch loop does NOT clear initial_target).
        // was_disconnected stays true, initial_target stays Some.

        // Second Connected → still RejoinInitial (retry).
        assert_eq!(
            classify(&connected, &initial_target, false, true),
            WatchDecision::RejoinInitial("target-uuid".to_string())
        );

        // Simulate rejoin success (watch loop clears initial_target).
        initial_target = None;

        // Third Connected without session → NoOp.
        assert_eq!(
            classify(&connected, &initial_target, false, false),
            WatchDecision::NoOp
        );
    }
}
