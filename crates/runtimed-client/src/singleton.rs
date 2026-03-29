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
