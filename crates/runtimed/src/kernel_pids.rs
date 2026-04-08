//! Persist kernel process group IDs to disk for orphan reaping.
//!
//! When runtimed is killed with SIGKILL, its graceful shutdown code never runs.
//! Kernel processes spawned with `process_group(0)` survive as orphans. This
//! module persists kernel pgids to `kernels.json` so the next daemon instance
//! can reap them on startup.
//!
//! Connection files are colocated in `connections_dir()` alongside this registry.
//! Together they contain enough information to either kill orphaned kernels (the
//! current strategy) or, in a future iteration, reattach to still-running kernels
//! during a daemon upgrade (pgid to verify liveness, connection file for ZMQ ports).
//!
//! All I/O is synchronous (`std::fs`) since it's called from both async and
//! `Drop` contexts. The registry is only accessed by a single daemon process,
//! so no file locking is needed — Tokio's cooperative scheduling ensures the
//! synchronous read-modify-write won't interleave between tasks.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::error;
#[cfg(unix)]
use tracing::{info, warn};

use crate::daemon_base_dir;

#[derive(Debug, Serialize, Deserialize)]
struct KernelEntry {
    pgid: i32,
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

/// Register a kernel's process group ID for orphan tracking.
pub fn register_kernel(kernel_id: &str, pgid: i32) {
    let mut registry = read_registry().unwrap_or(KernelRegistry {
        kernels: HashMap::new(),
    });

    registry
        .kernels
        .insert(kernel_id.to_string(), KernelEntry { pgid });

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
        let _ = std::fs::remove_file(registry_path());
    } else {
        write_registry(&registry);
    }
}

/// Reap orphaned kernel process groups from a previous daemon instance.
///
/// Reads `kernels.json`, sends SIGKILL to each process group, bulk-cleans
/// leftover connection files from `connections_dir()`, and removes the
/// registry. Returns the number of process groups successfully killed.
///
/// This runs at daemon startup before any new kernels are launched, so all
/// files in `connections_dir()` are guaranteed to be orphaned from a previous
/// run and safe to delete.
///
/// Currently this always kills orphaned kernels. A future "reattach" strategy
/// could instead verify kernel liveness and reconnect via the connection files,
/// enabling daemon upgrades without interrupting running kernels.
#[cfg(unix)]
pub fn reap_orphaned_kernels() -> usize {
    let registry = match read_registry() {
        Some(r) => r,
        None => {
            // No registry, but there may still be leftover connection files
            // from a previous version that didn't clean up properly.
            clean_connection_files();
            return 0;
        }
    };

    if registry.kernels.is_empty() {
        let _ = std::fs::remove_file(registry_path());
        clean_connection_files();
        return 0;
    }

    let mut reaped = 0;
    let mut failed_entries: HashMap<String, KernelEntry> = HashMap::new();

    for (kernel_id, entry) in registry.kernels {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if entry.pgid <= 0 {
            warn!(
                "[kernel-pids] Skipping invalid pgid {} for kernel '{}'",
                entry.pgid, kernel_id
            );
            continue;
        }

        let should_remove = match killpg(Pid::from_raw(entry.pgid), Signal::SIGKILL) {
            Ok(()) => {
                info!(
                    "[kernel-pids] Reaped orphaned kernel '{}' (pgid {})",
                    kernel_id, entry.pgid
                );
                reaped += 1;
                true
            }
            Err(nix::errno::Errno::ESRCH) => {
                info!(
                    "[kernel-pids] Orphaned kernel '{}' (pgid {}) already dead",
                    kernel_id, entry.pgid
                );
                true
            }
            Err(nix::errno::Errno::EPERM) => {
                warn!(
                    "[kernel-pids] Permission denied killing orphaned kernel '{}' (pgid {}), \
                     removing stale entry from registry",
                    kernel_id, entry.pgid
                );
                true
            }
            Err(e) => {
                error!(
                    "[kernel-pids] Failed to kill orphaned kernel '{}' (pgid {}): {}",
                    kernel_id, entry.pgid, e
                );
                false
            }
        };

        if !should_remove {
            failed_entries.insert(kernel_id, entry);
        }
    }

    // Rewrite registry with only the failed entries, or remove it entirely
    if failed_entries.is_empty() {
        let _ = std::fs::remove_file(registry_path());
    } else {
        write_registry(&KernelRegistry {
            kernels: failed_entries,
        });
    }

    // Bulk-clean all leftover connection files. Safe because this runs at
    // startup before any new kernels are launched.
    clean_connection_files();

    reaped
}

/// Remove all files in the connections directory.
#[cfg(unix)]
fn clean_connection_files() {
    let conn_dir = crate::connections_dir();
    if conn_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&conn_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
