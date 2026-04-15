//! Daemon discovery — read daemon info from the info file.
//!
//! The write-side (`DaemonLock`) lives in the `runtimed` crate since only
//! the daemon process acquires the lock.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Information about a running daemon instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// Socket endpoint the daemon is listening on.
    pub endpoint: String,
    /// Process ID of the daemon.
    pub pid: u32,
    /// Version of the daemon.
    pub version: String,
    /// When the daemon started.
    pub started_at: DateTime<Utc>,
    /// HTTP port for the blob server (if running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_port: Option<u16>,
    /// Path to the git worktree (dev mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Human-readable workspace description (dev mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_description: Option<String>,
}

/// Get the path to the daemon lock file.
pub fn daemon_lock_path() -> PathBuf {
    crate::daemon_base_dir().join("daemon.lock")
}

/// Get the path to the daemon info file.
pub fn daemon_info_path() -> PathBuf {
    crate::daemon_base_dir().join("daemon.json")
}

/// Read daemon info from the info file.
pub fn read_daemon_info(path: &PathBuf) -> Option<DaemonInfo> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Check if a daemon is running by reading the info file.
pub fn get_running_daemon_info() -> Option<DaemonInfo> {
    read_daemon_info(&daemon_info_path())
}

/// Get daemon info, preferring the socket-based `GetDaemonInfo` request
/// and falling back to the on-disk `daemon.json` sidecar.
///
/// This is the intended replacement for `get_running_daemon_info()` —
/// the socket is the source of truth (the daemon fills the response
/// from live state, so the result can't be stale the way the file
/// can). The file is read only when the socket query fails, which
/// happens when an older daemon doesn't recognise the new request.
/// Once every daemon in the wild knows `GetDaemonInfo`, the fallback
/// (and the whole file) can be deleted.
///
/// `socket_path` pins which daemon is being queried. The fallback
/// reads `daemon.json` from the **same directory as the socket**, so
/// callers that pin a non-default daemon (tests, worktree isolation,
/// cross-channel lookups) still resolve to the correct instance — not
/// the process's default namespace.
pub async fn query_daemon_info(socket_path: std::path::PathBuf) -> Option<DaemonInfo> {
    let client = crate::client::PoolClient::new(socket_path.clone());
    if let Ok(info) = client.daemon_info().await {
        return Some(DaemonInfo {
            endpoint: socket_path.to_string_lossy().into_owned(),
            pid: info.pid,
            version: info.daemon_version,
            started_at: info.started_at,
            blob_port: info.blob_port,
            worktree_path: info.worktree_path,
            workspace_description: info.workspace_description,
        });
    }

    // `GetDaemonInfo` failed. Only fall back to `daemon.json` when the
    // failure means "old daemon doesn't recognise this request." We
    // distinguish that from a generic transient failure by sending a
    // `Ping`: an old daemon will respond to Ping fine but tear down
    // the connection on `GetDaemonInfo` (unknown serde tag → drop).
    // A genuinely unreachable daemon fails Ping too, and we return
    // None so callers don't consume a stale sidecar.
    if client.ping().await.is_err() {
        return None;
    }

    // Daemon is alive but doesn't know `GetDaemonInfo` — legacy path.
    // The base notebook app case (nightly app ↔ nightly daemon) has the
    // socket and `daemon.json` in the same `daemon_base_dir()`, so the
    // colocated path covers every realistic upgrade-window configuration.
    // Windows named pipes have no on-disk parent — skip the fallback,
    // because this whole path is a one-release compatibility shim and
    // we don't have Windows daemons pre-GetDaemonInfo in the wild anyway.
    #[cfg(unix)]
    {
        let parent = socket_path.parent()?;
        read_daemon_info(&parent.join("daemon.json"))
    }
    #[cfg(not(unix))]
    {
        let _ = &socket_path;
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_paths() {
        let lock_path = daemon_lock_path();
        let info_path = daemon_info_path();

        assert!(lock_path.to_string_lossy().contains("runt"));
        assert!(lock_path.to_string_lossy().contains("daemon.lock"));
        assert!(info_path.to_string_lossy().contains("daemon.json"));
    }
}
