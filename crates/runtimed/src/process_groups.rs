//! Persist runtime agent process group IDs to disk for orphan reaping.
//!
//! When runtimed is killed with SIGKILL, its graceful shutdown code never runs.
//! Runtime agent processes spawned with `process_group(0)` survive as orphans.
//! This module persists agent pgids to `agents.json` so the next daemon instance
//! can reap them on startup.
//!
//! Connection files are colocated in `connections_dir()` alongside this registry.
//! Together they contain enough information to either kill orphaned agents (the
//! current strategy) or, in a future iteration, reattach to still-running agents
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
struct AgentEntry {
    pgid: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentRegistry {
    agents: HashMap<String, AgentEntry>,
}

fn registry_path() -> PathBuf {
    daemon_base_dir().join("agents.json")
}

fn read_registry() -> Option<AgentRegistry> {
    let path = registry_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_registry(registry: &AgentRegistry) {
    let path = registry_path();
    let tmp_path = path.with_extension("json.tmp");

    let data = match serde_json::to_string_pretty(registry) {
        Ok(d) => d,
        Err(e) => {
            error!("[process-groups] Failed to serialize registry: {}", e);
            return;
        }
    };

    if let Err(e) = std::fs::write(&tmp_path, &data) {
        error!("[process-groups] Failed to write temp file: {}", e);
        return;
    }

    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        error!("[process-groups] Failed to rename temp file: {}", e);
    }
}

/// Register an agent's process group ID for orphan tracking.
pub fn register_agent(agent_id: &str, pgid: i32) {
    let mut registry = read_registry().unwrap_or(AgentRegistry {
        agents: HashMap::new(),
    });

    registry
        .agents
        .insert(agent_id.to_string(), AgentEntry { pgid });

    write_registry(&registry);
}

/// Remove an agent from the registry (called during graceful shutdown).
pub fn unregister_agent(agent_id: &str) {
    let mut registry = match read_registry() {
        Some(r) => r,
        None => return,
    };

    registry.agents.remove(agent_id);

    if registry.agents.is_empty() {
        let _ = std::fs::remove_file(registry_path());
    } else {
        write_registry(&registry);
    }
}

// ============================================================================
// Legacy migration: reap orphaned process groups from old `kernels.json`
// ============================================================================

/// Path to the legacy `kernels.json` registry from previous daemon versions.
#[cfg(unix)]
fn legacy_registry_path() -> PathBuf {
    daemon_base_dir().join("kernels.json")
}

/// Legacy registry format: `{ "kernels": { "id": { "pgid": N } } }`.
#[cfg(unix)]
#[derive(Deserialize)]
struct LegacyRegistry {
    kernels: HashMap<String, AgentEntry>,
}

/// Reap orphaned process groups from a legacy `kernels.json` and delete it.
///
/// Returns the number of process groups successfully killed. If the file
/// doesn't exist or can't be parsed, returns 0 and does nothing.
#[cfg(unix)]
fn reap_legacy_registry() -> usize {
    let path = legacy_registry_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return 0,
    };

    let legacy: LegacyRegistry = match serde_json::from_str(&data) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "[process-groups] Failed to parse legacy kernels.json, removing: {}",
                e
            );
            let _ = std::fs::remove_file(&path);
            return 0;
        }
    };

    if legacy.kernels.is_empty() {
        let _ = std::fs::remove_file(&path);
        return 0;
    }

    let mut reaped = 0;

    for (id, entry) in &legacy.kernels {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if entry.pgid <= 0 {
            warn!(
                "[process-groups] Skipping invalid pgid {} in legacy entry '{}'",
                entry.pgid, id
            );
            continue;
        }

        match killpg(Pid::from_raw(entry.pgid), Signal::SIGKILL) {
            Ok(()) => {
                info!(
                    "[process-groups] Reaped legacy orphaned process '{}' (pgid {})",
                    id, entry.pgid
                );
                reaped += 1;
            }
            Err(nix::errno::Errno::ESRCH) => {
                info!(
                    "[process-groups] Legacy orphaned process '{}' (pgid {}) already dead",
                    id, entry.pgid
                );
            }
            Err(e) => {
                warn!(
                    "[process-groups] Failed to kill legacy process '{}' (pgid {}): {}",
                    id, entry.pgid, e
                );
            }
        }
    }

    // Always remove the legacy file — we don't preserve failed entries
    let _ = std::fs::remove_file(&path);
    reaped
}

/// Reap orphaned agent process groups from a previous daemon instance.
///
/// First migrates any leftover `kernels.json` from previous daemon versions,
/// then reads `agents.json`, sends SIGKILL to each process group, bulk-cleans
/// leftover connection files from `connections_dir()`, and removes the
/// registry. Returns the number of process groups successfully killed.
///
/// This runs at daemon startup before any new agents are launched, so all
/// files in `connections_dir()` are guaranteed to be orphaned from a previous
/// run and safe to delete.
#[cfg(unix)]
pub fn reap_orphaned_agents() -> usize {
    // Migrate legacy kernels.json first
    let mut reaped = reap_legacy_registry();

    let registry = match read_registry() {
        Some(r) => r,
        None => {
            // No registry, but there may still be leftover connection files
            // from a previous version that didn't clean up properly.
            clean_connection_files();
            return reaped;
        }
    };

    if registry.agents.is_empty() {
        let _ = std::fs::remove_file(registry_path());
        clean_connection_files();
        return reaped;
    }

    let mut failed_entries: HashMap<String, AgentEntry> = HashMap::new();

    for (agent_id, entry) in registry.agents {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if entry.pgid <= 0 {
            warn!(
                "[process-groups] Skipping invalid pgid {} for agent '{}'",
                entry.pgid, agent_id
            );
            continue;
        }

        let should_remove = match killpg(Pid::from_raw(entry.pgid), Signal::SIGKILL) {
            Ok(()) => {
                info!(
                    "[process-groups] Reaped orphaned agent '{}' (pgid {})",
                    agent_id, entry.pgid
                );
                reaped += 1;
                true
            }
            Err(nix::errno::Errno::ESRCH) => {
                info!(
                    "[process-groups] Orphaned agent '{}' (pgid {}) already dead",
                    agent_id, entry.pgid
                );
                true
            }
            Err(nix::errno::Errno::EPERM) => {
                warn!(
                    "[process-groups] Permission denied killing orphaned agent '{}' (pgid {}), \
                     removing stale entry from registry",
                    agent_id, entry.pgid
                );
                true
            }
            Err(e) => {
                error!(
                    "[process-groups] Failed to kill orphaned agent '{}' (pgid {}): {}",
                    agent_id, entry.pgid, e
                );
                false
            }
        };

        if !should_remove {
            failed_entries.insert(agent_id, entry);
        }
    }

    // Rewrite registry with only the failed entries, or remove it entirely
    if failed_entries.is_empty() {
        let _ = std::fs::remove_file(registry_path());
    } else {
        write_registry(&AgentRegistry {
            agents: failed_entries,
        });
    }

    // Bulk-clean all leftover connection files. Safe because this runs at
    // startup before any new agents are launched.
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
