//! Persist kernel process group IDs to disk for orphan reaping.
//!
//! When runtimed is killed with SIGKILL, its graceful shutdown code never runs.
//! Kernel processes spawned with `process_group(0)` survive as orphans. This
//! module persists kernel pgids to `kernels.json` so the next daemon instance
//! can reap them on startup.
//!
//! All I/O is synchronous (`std::fs`) since it's called from both async and
//! `Drop` contexts.

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::daemon_base_dir;

#[derive(Debug, Serialize, Deserialize)]
struct KernelEntry {
    pgid: i32,
    connection_file: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct KernelRegistry {
    kernels: HashMap<String, KernelEntry>,
}

fn registry_path() -> PathBuf {
    daemon_base_dir().join("kernels.json")
}

fn read_registry() -> Option<KernelRegistry> {
    let path = registry_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_registry(registry: &KernelRegistry) {
    let path = registry_path();
    let tmp_path = path.with_extension("json.tmp");

    let data = match serde_json::to_string_pretty(registry) {
        Ok(d) => d,
        Err(e) => {
            error!("[kernel-pids] Failed to serialize registry: {}", e);
            return;
        }
    };

    if let Err(e) = std::fs::write(&tmp_path, &data) {
        error!("[kernel-pids] Failed to write temp file: {}", e);
        return;
    }

    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        error!("[kernel-pids] Failed to rename temp file: {}", e);
    }
}

/// Register a kernel's process group ID and connection file path.
pub fn register_kernel(kernel_id: &str, pgid: i32, connection_file_path: &std::path::Path) {
    let mut registry = read_registry().unwrap_or(KernelRegistry {
        kernels: HashMap::new(),
    });

    registry.kernels.insert(
        kernel_id.to_string(),
        KernelEntry {
            pgid,
            connection_file: connection_file_path.to_string_lossy().to_string(),
        },
    );

    write_registry(&registry);
}

/// Remove a kernel from the registry (called during graceful shutdown).
pub fn unregister_kernel(kernel_id: &str) {
    let mut registry = match read_registry() {
        Some(r) => r,
        None => return,
    };

    registry.kernels.remove(kernel_id);

    if registry.kernels.is_empty() {
        // Clean up the file entirely when no kernels remain
        let _ = std::fs::remove_file(registry_path());
    } else {
        write_registry(&registry);
    }
}

/// Reap orphaned kernel process groups from a previous daemon instance.
///
/// Reads `kernels.json`, sends SIGKILL to each process group, cleans up
/// connection files, and deletes the registry. Returns the number of
/// process groups successfully killed.
#[cfg(unix)]
pub fn reap_orphaned_kernels() -> usize {
    let registry = match read_registry() {
        Some(r) => r,
        None => return 0,
    };

    if registry.kernels.is_empty() {
        let _ = std::fs::remove_file(registry_path());
        return 0;
    }

    let mut reaped = 0;

    for (kernel_id, entry) in &registry.kernels {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        match killpg(Pid::from_raw(entry.pgid), Signal::SIGKILL) {
            Ok(()) => {
                info!(
                    "[kernel-pids] Reaped orphaned kernel '{}' (pgid {})",
                    kernel_id, entry.pgid
                );
                reaped += 1;
            }
            Err(nix::errno::Errno::ESRCH) => {
                // Process group already dead — not an error
                info!(
                    "[kernel-pids] Orphaned kernel '{}' (pgid {}) already dead",
                    kernel_id, entry.pgid
                );
            }
            Err(nix::errno::Errno::EPERM) => {
                warn!(
                    "[kernel-pids] No permission to kill orphaned kernel '{}' (pgid {})",
                    kernel_id, entry.pgid
                );
            }
            Err(e) => {
                error!(
                    "[kernel-pids] Failed to kill orphaned kernel '{}' (pgid {}): {}",
                    kernel_id, entry.pgid, e
                );
            }
        }

        // Clean up connection file
        let conn_path = std::path::Path::new(&entry.connection_file);
        if conn_path.exists() {
            if let Err(e) = std::fs::remove_file(conn_path) {
                warn!(
                    "[kernel-pids] Failed to remove connection file {}: {}",
                    entry.connection_file, e
                );
            }
        }
    }

    // Remove the registry file
    let _ = std::fs::remove_file(registry_path());

    reaped
}
