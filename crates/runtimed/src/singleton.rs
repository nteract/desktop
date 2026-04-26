//! Singleton management for the pool daemon.
//!
//! Ensures only one daemon instance runs per user using file-based locking.
//! Read-only daemon discovery (DaemonInfo, get_running_daemon_info, etc.)
//! lives in `runtimed_client::singleton` and is re-exported via `pub use runtimed_client::*`.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use chrono::Utc;
use tracing::{info, warn};

// Re-export all client-side singleton items so `runtimed::singleton::*` still works.
use runtimed_client::singleton as client_singleton;
pub use runtimed_client::singleton::{
    daemon_info_path, daemon_lock_path, get_running_daemon_info, read_daemon_info, DaemonInfo,
};

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
    pub fn try_acquire(
        custom_lock_dir: Option<&PathBuf>,
    ) -> Result<Self, Box<client_singleton::DaemonInfo>> {
        let (lock_path, info_path) = if let Some(dir) = custom_lock_dir {
            (dir.join("daemon.lock"), dir.join("daemon.json"))
        } else {
            (
                client_singleton::daemon_lock_path(),
                client_singleton::daemon_info_path(),
            )
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
                if let Some(info) = client_singleton::read_daemon_info(&info_path) {
                    return Err(Box::new(info));
                }
                // No info available, create a placeholder
                return Err(Box::new(client_singleton::DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    execution_store_dir: None,
                    worktree_path: None,
                    workspace_description: None,
                }));
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
                if let Some(info) = client_singleton::read_daemon_info(&info_path) {
                    return Err(Box::new(info));
                }
                return Err(Box::new(client_singleton::DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    execution_store_dir: None,
                    worktree_path: None,
                    workspace_description: None,
                }));
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
                if let Some(info) = client_singleton::read_daemon_info(&info_path) {
                    return Err(Box::new(info));
                }
                return Err(Box::new(client_singleton::DaemonInfo {
                    endpoint: "unknown".to_string(),
                    pid: 0,
                    version: "unknown".to_string(),
                    started_at: Utc::now(),
                    blob_port: None,
                    execution_store_dir: None,
                    worktree_path: None,
                    workspace_description: None,
                }));
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

        let info = client_singleton::DaemonInfo {
            endpoint: endpoint.to_string(),
            pid: std::process::id(),
            version: crate::daemon_version().to_string(),
            started_at: Utc::now(),
            blob_port,
            execution_store_dir: Some(
                crate::default_execution_store_dir()
                    .to_string_lossy()
                    .to_string(),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn try_acquire_succeeds_in_empty_dir() -> TestResult {
        // Fresh directory with no existing daemon → we should be the singleton.
        let tmp = TempDir::new()?;
        let dir = tmp.path().to_path_buf();
        let lock = DaemonLock::try_acquire(Some(&dir)).map_err(|_| "should acquire empty dir")?;
        assert_eq!(lock.info_path(), &dir.join("daemon.json"));
        Ok(())
    }

    #[test]
    fn try_acquire_conflicts_when_lock_held() -> TestResult {
        // Second acquire on the same directory must fail while the first
        // lock is alive. This is the whole point of the singleton —
        // regressing it would allow concurrent daemons stomping on each
        // other's sockets.
        let tmp = TempDir::new()?;
        let dir = tmp.path().to_path_buf();
        let first = DaemonLock::try_acquire(Some(&dir)).map_err(|_| "first acquire")?;
        first.write_info("unix:/tmp/sock", Some(12345))?;

        let Err(info) = DaemonLock::try_acquire(Some(&dir)) else {
            return Err("second acquire should have failed".into());
        };
        // Second acquirer reads the on-disk info so callers can surface a
        // useful message ("endpoint X, pid Y is already running").
        assert_eq!(info.endpoint, "unix:/tmp/sock");
        assert_eq!(info.blob_port, Some(12345));
        Ok(())
    }

    #[test]
    fn drop_releases_lock_and_removes_info() -> TestResult {
        // After the first lock drops, a second acquire must succeed AND
        // the stale daemon.json must be gone (so clients don't connect
        // to a dead endpoint).
        let tmp = TempDir::new()?;
        let dir = tmp.path().to_path_buf();
        {
            let lock = DaemonLock::try_acquire(Some(&dir)).map_err(|_| "first acquire")?;
            lock.write_info("unix:/tmp/old", None)?;
            assert!(dir.join("daemon.json").exists());
        }
        // After drop, info file gone.
        assert!(!dir.join("daemon.json").exists());
        // And we can re-acquire.
        let _second = DaemonLock::try_acquire(Some(&dir)).map_err(|_| "re-acquire after drop")?;
        Ok(())
    }

    #[test]
    fn write_info_roundtrips_through_read_daemon_info() -> TestResult {
        let tmp = TempDir::new()?;
        let dir = tmp.path().to_path_buf();
        let lock = DaemonLock::try_acquire(Some(&dir)).map_err(|_| "acquire")?;
        lock.write_info("unix:/tmp/rt.sock", Some(4242))?;

        let info =
            client_singleton::read_daemon_info(&dir.join("daemon.json")).ok_or("read back info")?;
        assert_eq!(info.endpoint, "unix:/tmp/rt.sock");
        assert_eq!(info.blob_port, Some(4242));
        assert_eq!(info.pid, std::process::id());
        assert!(!info.version.is_empty());
        Ok(())
    }

    #[test]
    fn try_acquire_creates_parent_dir() -> TestResult {
        // The daemon base dir may not exist yet on first run. Acquire
        // must create it rather than failing with ENOENT.
        let tmp = TempDir::new()?;
        let nested = tmp.path().join("deep/nested/dir");
        assert!(!nested.exists());
        let _lock =
            DaemonLock::try_acquire(Some(&nested)).map_err(|_| "acquire in nonexistent dir")?;
        assert!(nested.exists());
        assert!(nested.join("daemon.lock").exists());
        Ok(())
    }
}
