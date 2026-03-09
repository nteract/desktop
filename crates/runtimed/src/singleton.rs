//! Singleton management for the pool daemon.
//!
//! Ensures only one daemon instance runs per user using file-based locking.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use log::{info, warn};
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

/// A lock that ensures only one daemon instance runs.
pub struct DaemonLock {
    _lock_file: File,
    _lock_path: PathBuf,
    info_path: PathBuf,
}

impl DaemonLock {
    /// Attempt to acquire the daemon lock.
    ///
    /// Returns `Ok(lock)` if we acquired the lock (we are the singleton).
    /// Returns `Err(info)` if another daemon is running (with its info).
    ///
    /// If `custom_lock_dir` is provided, uses that directory for lock files
    /// instead of the default. This is primarily for testing.
    pub fn try_acquire(custom_lock_dir: Option<&PathBuf>) -> Result<Self, DaemonInfo> {
        let (lock_path, info_path) = if let Some(dir) = custom_lock_dir {
            (dir.join("daemon.lock"), dir.join("daemon.json"))
        } else {
            (daemon_lock_path(), daemon_info_path())
        };

        // Ensure parent directory exists
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        // Try to open/create the lock file
        let lock_file = match OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) => {
                warn!("[singleton] Failed to open lock file: {}", e);
                // Try to read existing daemon info
                if let Some(info) = read_daemon_info(&info_path) {
                    return Err(info);
                }
                // No info available, create a placeholder
                return Err(DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    worktree_path: None,
                    workspace_description: None,
                });
            }
        };

        // Try to acquire exclusive lock (non-blocking)
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = lock_file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                // Another process holds the lock
                info!("[singleton] Another daemon is already running");
                if let Some(info) = read_daemon_info(&info_path) {
                    return Err(info);
                }
                return Err(DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    worktree_path: None,
                    workspace_description: None,
                });
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::Storage::FileSystem::{
                LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            };

            let handle = lock_file.as_raw_handle() as HANDLE;
            // SAFETY: zeroed is valid for OVERLAPPED struct
            let mut overlapped = unsafe { std::mem::zeroed() };
            let result = unsafe {
                LockFileEx(
                    handle,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if result == 0 {
                info!("[singleton] Another daemon is already running");
                if let Some(info) = read_daemon_info(&info_path) {
                    return Err(info);
                }
                return Err(DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    worktree_path: None,
                    workspace_description: None,
                });
            }
        }

        info!("[singleton] Acquired daemon lock");

        Ok(Self {
            _lock_file: lock_file,
            _lock_path: lock_path,
            info_path,
        })
    }

    /// Write daemon info after successful startup.
    pub fn write_info(&self, endpoint: &str, blob_port: Option<u16>) -> std::io::Result<()> {
        // Populate worktree info when in dev mode
        let (worktree_path, workspace_description) = if crate::is_dev_mode() {
            (
                crate::get_workspace_path().map(|p| p.to_string_lossy().to_string()),
                crate::get_workspace_name(),
            )
        } else {
            (None, None)
        };

        let info = DaemonInfo {
            endpoint: endpoint.to_string(),
            pid: std::process::id(),
            version: crate::daemon_version().to_string(),
            started_at: Utc::now(),
            blob_port,
            worktree_path,
            workspace_description,
        };

        let json = serde_json::to_string_pretty(&info).map_err(std::io::Error::other)?;

        std::fs::write(&self.info_path, json)?;
        info!("[singleton] Wrote daemon info to {:?}", self.info_path);

        Ok(())
    }

    /// Get the path to the info file.
    pub fn info_path(&self) -> &PathBuf {
        &self.info_path
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Clean up info file when daemon exits
        if self.info_path.exists() {
            std::fs::remove_file(&self.info_path).ok();
        }
        info!("[singleton] Released daemon lock");
    }
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
