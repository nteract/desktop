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
    /// Nothing to do.
    NoOp,
}

/// Classify a `DaemonEvent` into the action the watch loop should take.
///
/// `initial_target` is consumed on the first event that triggers a rejoin
/// so the seeded hand-off from the proxy only applies once.
fn classify(
    event: &DaemonEvent,
    initial_target: &mut Option<String>,
    has_session: bool,
) -> WatchDecision {
    match event {
        DaemonEvent::Upgraded { previous, current } => {
            if previous.version != current.version {
                return WatchDecision::Exit(EXIT_DAEMON_UPGRADED);
            }
            if let Some(t) = initial_target.take() {
                WatchDecision::RejoinInitial(t)
            } else if has_session {
                WatchDecision::RejoinContinuation
            } else {
                WatchDecision::NoOp
            }
        }
        DaemonEvent::Connected { .. } => {
            if let Some(t) = initial_target.take() {
                WatchDecision::RejoinInitial(t)
            } else if has_session {
                WatchDecision::RejoinContinuation
            } else {
                WatchDecision::NoOp
            }
        }
        DaemonEvent::Disconnected => WatchDecision::NoOp,
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

    loop {
        let event = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Daemon event stream lagged, dropped {n} events");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return 0,
        };

        let has_session = session.read().await.is_some();
        match classify(&event, &mut initial_target, has_session) {
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
                rejoin(&socket_path, &session, &peer_label, Some(target)).await;
            }
            WatchDecision::RejoinContinuation => {
                info!("Daemon reachable, rejoining notebook session");
                rejoin(&socket_path, &session, &peer_label, None).await;
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
/// For ephemeral notebooks, uses `connect(uuid)` and detects data loss
/// (empty document after reconnect means the daemon evicted the room).
/// When this happens, the session is cleared so the watch loop stops
/// trying to rejoin — without this, the 10s reconnect cycle would
/// perpetually recreate peers and prevent proper room eviction (#2088).
async fn rejoin(
    socket_path: &Path,
    session: &Arc<RwLock<Option<NotebookSession>>>,
    peer_label: &Arc<RwLock<String>>,
    override_target: Option<String>,
) {
    let (notebook_id, notebook_path, prev_cell_count) = match override_target {
        Some(target) if looks_like_uuid(&target) => (target, None, 0),
        Some(target) => {
            // Treat as file path. We'll learn the real notebook_id from
            // connect_open's response.
            (target.clone(), Some(target), 0)
        }
        None => {
            let guard = session.read().await;
            match guard.as_ref() {
                Some(s) => (
                    s.notebook_id.clone(),
                    s.notebook_path.clone(),
                    s.handle.get_cell_ids().len(),
                ),
                None => return,
            }
        }
    };

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
                if prev_cell_count > 0 && new_cell_count == 0 && notebook_path.is_none() {
                    warn!(
                        "Ephemeral notebook lost: rejoined {notebook_id} but document is empty \
                         (had {prev_cell_count} cells). Clearing session to stop reconnect loop."
                    );
                    // Clear the session so the watch loop stops trying to
                    // rejoin an evicted ephemeral notebook. Without this,
                    // every 10s daemon_watch reconnects, briefly creates a
                    // peer, detects the empty doc, and drops — but the
                    // session stays `Some`, so `has_session` remains true and
                    // the cycle repeats, preventing proper eviction (#2088).
                    *session.write().await = None;
                    return;
                }

                crate::presence::announce(&handle, &label).await;

                let new_session = NotebookSession {
                    handle,
                    broadcast_rx,
                    notebook_id: new_notebook_id,
                    notebook_path: notebook_path.clone(),
                };
                *session.write().await = Some(new_session);
                info!("Rejoined notebook session ({new_cell_count} cells)");
                return;
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
        let mut initial = None;
        assert_eq!(
            classify(&event, &mut initial, false),
            WatchDecision::Exit(EXIT_DAEMON_UPGRADED)
        );
        assert!(initial.is_none(), "initial target should not be consumed");
    }

    #[test]
    fn same_version_restart_triggers_continuation_rejoin() {
        let event = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        let mut initial = None;
        assert_eq!(
            classify(&event, &mut initial, true),
            WatchDecision::RejoinContinuation
        );
    }

    #[test]
    fn same_version_restart_without_session_is_noop() {
        let event = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        let mut initial = None;
        assert_eq!(classify(&event, &mut initial, false), WatchDecision::NoOp);
    }

    #[test]
    fn connected_consumes_initial_target_once() {
        let event = DaemonEvent::Connected {
            info: info_with("1.0.0", 100),
        };
        let mut initial = Some("abc-uuid".to_string());
        assert_eq!(
            classify(&event, &mut initial, false),
            WatchDecision::RejoinInitial("abc-uuid".to_string())
        );
        assert!(initial.is_none(), "initial target must be consumed");

        // Second Connected without a session should now be a no-op.
        assert_eq!(classify(&event, &mut initial, false), WatchDecision::NoOp);
    }

    #[test]
    fn disconnected_is_always_noop() {
        let mut initial = Some("abc".to_string());
        assert_eq!(
            classify(&DaemonEvent::Disconnected, &mut initial, true),
            WatchDecision::NoOp
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
        let mut initial = None;

        // With has_session = true, we get RejoinContinuation.
        assert_eq!(
            classify(&event, &mut initial, true),
            WatchDecision::RejoinContinuation
        );

        // After the session is cleared (has_session = false), same event is NoOp.
        assert_eq!(classify(&event, &mut initial, false), WatchDecision::NoOp);

        // Same for Upgraded (same-version restart).
        let upgraded = DaemonEvent::Upgraded {
            previous: info_with("1.0.0", 100),
            current: info_with("1.0.0", 200),
        };
        assert_eq!(
            classify(&upgraded, &mut initial, false),
            WatchDecision::NoOp
        );
    }
}
