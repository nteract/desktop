//! Daemon health monitoring and reconnection.
//!
//! Spawns a background task that periodically pings the daemon. When the daemon
//! becomes unreachable, it transitions to `Reconnecting` with exponential backoff.
//! When the daemon returns:
//! - **Same version:** auto-rejoin the notebook session
//! - **Different version (upgrade):** exit so MCP clients restart with the new binary

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use runtimed_client::client::PoolClient;
use runtimed_client::singleton::{query_daemon_info, DaemonInfo};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::session::NotebookSession;

/// Exit code when the daemon has been upgraded and the MCP server should restart.
/// EX_TEMPFAIL (sysexits.h) — "temporary failure; try again."
pub const EXIT_DAEMON_UPGRADED: i32 = 75;

/// Current connection state to the daemon.
pub enum DaemonState {
    /// Connected and healthy.
    Connected { info: DaemonInfo },
    /// Daemon is unreachable; reconnecting with backoff.
    /// `last_info` is `None` when `runt mcp` started before the daemon was available.
    Reconnecting {
        since: Instant,
        attempt: u32,
        last_info: Option<DaemonInfo>,
    },
}

impl DaemonState {
    /// Human-readable status for tool error messages.
    pub fn reconnecting_message(&self) -> Option<String> {
        match self {
            DaemonState::Connected { .. } => None,
            DaemonState::Reconnecting { since, attempt, .. } => {
                let elapsed = since.elapsed().as_secs();
                Some(format!(
                    "Daemon is restarting (attempt {attempt}, {elapsed}s elapsed). \
                     Tools will resume automatically when the daemon is back."
                ))
            }
        }
    }
}

const PING_INTERVAL: Duration = Duration::from_secs(5);
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

fn backoff_duration(attempt: u32) -> Duration {
    let secs = BACKOFF_BASE
        .as_secs()
        .saturating_mul(1u64 << attempt.min(5));
    Duration::from_secs(secs).min(BACKOFF_CAP)
}

/// Run the daemon health monitor loop.
///
/// Returns `Ok(EXIT_DAEMON_UPGRADED)` when the daemon has been upgraded and the
/// process should exit. Never returns under normal reconnection — it runs until
/// the daemon is upgraded or the task is cancelled.
pub async fn daemon_health_monitor(
    socket_path: PathBuf,
    daemon_state: Arc<RwLock<DaemonState>>,
    session: Arc<RwLock<Option<NotebookSession>>>,
    peer_label: Arc<RwLock<String>>,
) -> i32 {
    let client = PoolClient::new(socket_path.clone());

    loop {
        // Determine sleep duration based on current state
        let sleep_duration = {
            let state = daemon_state.read().await;
            match &*state {
                DaemonState::Connected { .. } => PING_INTERVAL,
                DaemonState::Reconnecting { attempt, .. } => backoff_duration(*attempt),
            }
        };

        tokio::time::sleep(sleep_duration).await;

        match client.ping().await {
            Ok(()) => {
                // Fetch daemon info BEFORE acquiring the state lock — every
                // MCP tool call reads daemon_state in its hot path, and
                // holding the write lock across an awaited IPC would block
                // the tool surface whenever a GetDaemonInfo query is slow.
                let current_info = query_daemon_info(socket_path.clone()).await;

                let mut state = daemon_state.write().await;
                match &*state {
                    DaemonState::Connected { info } => {
                        if let Some(current) = current_info {
                            if current.version != info.version {
                                // Version changed — genuine upgrade, exit for new binary
                                info!(
                                    "Daemon upgraded while connected: {} → {}",
                                    info.version, current.version
                                );
                                return EXIT_DAEMON_UPGRADED;
                            }
                            if current.pid != info.pid {
                                // Same version, different PID — daemon restarted but is
                                // already reachable (this ping succeeded). Stay Connected
                                // so tools aren't blocked, and rejoin the session inline.
                                info!(
                                    "Daemon restarted (same version {}, PID {} → {}), rejoining session",
                                    info.version, info.pid, current.pid
                                );
                                *state = DaemonState::Connected {
                                    info: current.clone(),
                                };
                                drop(state);
                                auto_rejoin_session(&socket_path, &session, &peer_label).await;
                            }
                        }
                    }
                    DaemonState::Reconnecting {
                        since,
                        attempt,
                        last_info,
                    } => {
                        let elapsed = since.elapsed();

                        if let (Some(ref current), Some(ref last)) = (&current_info, last_info) {
                            if current.version != last.version {
                                info!(
                                    "Daemon upgraded: {} → {} (reconnected after {:.1}s, {} attempts)",
                                    last.version,
                                    current.version,
                                    elapsed.as_secs_f64(),
                                    attempt
                                );
                                return EXIT_DAEMON_UPGRADED;
                            }
                        }

                        // Same version (or first connect with no prior info) — connect.
                        // We need daemon info to enter Connected; if neither the info
                        // file nor last_info is available, stay in Reconnecting.
                        let Some(new_info) = current_info.or_else(|| last_info.clone()) else {
                            warn!("Daemon responds to ping but info file is missing, retrying");
                            continue;
                        };

                        if last_info.is_some() {
                            info!(
                                "Daemon reconnected after {:.1}s ({} attempts)",
                                elapsed.as_secs_f64(),
                                attempt
                            );
                        } else {
                            info!(
                                "Daemon became available after {:.1}s ({} attempts)",
                                elapsed.as_secs_f64(),
                                attempt
                            );
                        }

                        let should_rejoin = last_info.is_some();
                        *state = DaemonState::Connected { info: new_info };

                        // Drop the state lock before auto-rejoin
                        drop(state);

                        // Auto-rejoin notebook session if daemon was previously connected
                        if should_rejoin {
                            auto_rejoin_session(&socket_path, &session, &peer_label).await;
                        }
                    }
                }
            }
            Err(e) => {
                let mut state = daemon_state.write().await;
                match &*state {
                    DaemonState::Connected { info } => {
                        warn!("Daemon ping failed, entering reconnect mode: {e}");
                        *state = DaemonState::Reconnecting {
                            since: Instant::now(),
                            attempt: 1,
                            last_info: Some(info.clone()),
                        };
                    }
                    DaemonState::Reconnecting {
                        since,
                        attempt,
                        last_info,
                    } => {
                        let new_attempt = attempt.saturating_add(1);
                        let elapsed = since.elapsed();
                        let next_backoff = backoff_duration(new_attempt);
                        warn!(
                            "Daemon still unreachable (attempt {new_attempt}, {:.1}s elapsed, next retry in {:.1}s): {e}",
                            elapsed.as_secs_f64(),
                            next_backoff.as_secs_f64(),
                        );
                        *state = DaemonState::Reconnecting {
                            since: *since,
                            attempt: new_attempt,
                            last_info: last_info.clone(),
                        };
                    }
                }
            }
        }
    }
}

/// Maximum retries for auto-rejoin. The daemon may answer pings before notebook
/// connections are fully ready, so we retry a few times with short delays.
const REJOIN_MAX_RETRIES: u32 = 3;
const REJOIN_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Attempt to re-join the active notebook session after daemon reconnection.
///
/// Retries up to [`REJOIN_MAX_RETRIES`] times with [`REJOIN_RETRY_DELAY`] between
/// attempts, since the daemon may respond to pings before it is ready to accept
/// notebook sync connections.
async fn auto_rejoin_session(
    socket_path: &Path,
    session: &Arc<RwLock<Option<NotebookSession>>>,
    peer_label: &Arc<RwLock<String>>,
) {
    // Read the current session's notebook_id
    let notebook_id = {
        let s = session.read().await;
        s.as_ref().map(|s| s.notebook_id.clone())
    };

    let Some(notebook_id) = notebook_id else {
        return; // No active session to rejoin
    };

    info!("Auto-rejoining notebook session: {notebook_id}");

    // Drop the old session first (its DocHandle/sync task are dead)
    *session.write().await = None;

    let label = peer_label.read().await.clone();

    for attempt in 0..=REJOIN_MAX_RETRIES {
        match notebook_sync::connect::connect(
            socket_path.to_path_buf(),
            notebook_id.clone(),
            &label,
        )
        .await
        {
            Ok(result) => {
                crate::presence::announce(&result.handle, &label).await;

                let new_session = NotebookSession {
                    handle: result.handle,
                    broadcast_rx: result.broadcast_rx,
                    notebook_id: notebook_id.clone(),
                };
                *session.write().await = Some(new_session);
                info!("Auto-rejoined notebook session: {notebook_id}");
                return;
            }
            Err(e) => {
                if attempt < REJOIN_MAX_RETRIES {
                    warn!(
                        "Failed to rejoin notebook {notebook_id} (attempt {}, retrying in {}s): {e}",
                        attempt + 1,
                        REJOIN_RETRY_DELAY.as_secs(),
                    );
                    tokio::time::sleep(REJOIN_RETRY_DELAY).await;
                } else {
                    warn!(
                        "Failed to auto-rejoin notebook {notebook_id} after {} attempts: {e}",
                        attempt + 1
                    );
                    // Session stays None — tools will prompt user to re-join
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_daemon_info() -> DaemonInfo {
        DaemonInfo {
            endpoint: "/tmp/test.sock".to_string(),
            pid: 1234,
            version: "1.0.0".to_string(),
            started_at: Utc::now(),
            blob_port: None,
            worktree_path: None,
            workspace_description: None,
        }
    }

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        assert_eq!(backoff_duration(0), Duration::from_secs(1));
        assert_eq!(backoff_duration(1), Duration::from_secs(2));
        assert_eq!(backoff_duration(2), Duration::from_secs(4));
        assert_eq!(backoff_duration(3), Duration::from_secs(8));
        assert_eq!(backoff_duration(4), Duration::from_secs(16));
        // Capped at BACKOFF_CAP (30s) from attempt 5 onward
        assert_eq!(backoff_duration(5), Duration::from_secs(30));
        assert_eq!(backoff_duration(6), Duration::from_secs(30));
        assert_eq!(backoff_duration(100), Duration::from_secs(30));
    }

    #[test]
    fn backoff_does_not_overflow() {
        // u32::MAX should not panic; saturating_mul handles overflow
        let d = backoff_duration(u32::MAX);
        assert!(d <= BACKOFF_CAP);
    }

    #[test]
    fn reconnecting_message_only_in_reconnecting_state() {
        let connected = DaemonState::Connected {
            info: test_daemon_info(),
        };
        assert!(connected.reconnecting_message().is_none());

        let reconnecting = DaemonState::Reconnecting {
            since: Instant::now(),
            attempt: 3,
            last_info: None,
        };
        if let Some(msg) = reconnecting.reconnecting_message() {
            assert!(msg.contains("attempt 3"));
            assert!(msg.contains("Daemon is restarting"));
        } else {
            panic!("Reconnecting state should produce a message");
        }
    }
}
