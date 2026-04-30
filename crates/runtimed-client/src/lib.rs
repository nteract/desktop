//! Client library for communicating with the runtimed daemon.
//!
//! This crate provides the client-facing API for runtimed: IPC client,
//! settings sync, daemon discovery, service management, and shared types.
//! It does NOT include any server-side daemon code or heavy dependencies
//! like kernel-env or kernel-launch.
//!
//! ## Crate consumers
//!
//! - `notebook` (Tauri app) — daemon lifecycle, settings sync
//! - `runt` (CLI) — daemon management commands
//! - `runtimed-py` (Python bindings) — PoolClient, SyncClient, settings
//! - `runtimed` (daemon) — re-exports everything, adds server-only code

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod client;
pub mod daemon_connection;
pub mod daemon_paths;
pub mod execution_store;
pub mod output_resolver;
pub mod resolved_output;
pub use notebook_doc;
pub use notebook_doc::metadata as notebook_metadata;
pub use notebook_doc::pool_state::{PoolState, RuntimePoolState};
/// Re-export connection types from notebook-protocol.
///
/// The canonical definitions live in `notebook_protocol::connection`.
/// This re-export preserves backward compatibility for existing callers.
pub use notebook_protocol::connection;
pub mod protocol;
pub mod runtime;
pub mod service;
pub mod settings_doc;
pub mod singleton;
pub mod sync_client;
pub mod telemetry;

// ============================================================================
// Development Mode and Worktree Isolation
// ============================================================================

// Re-export from runt-workspace for backwards compatibility
pub use runt_workspace::{
    build_channel, cache_namespace, cache_namespace_for, config_namespace, daemon_base_dir,
    daemon_base_dir_for, daemon_binary_basename, daemon_binary_basename_for, daemon_launchd_label,
    daemon_service_basename, default_notebook_log_path, default_socket_path, desktop_display_name,
    desktop_display_name_for, desktop_product_name, get_workspace_name, get_workspace_path,
    is_dev_mode, mcp_logs_dir, open_notebook_app_for_channel, session_state_path,
    settings_json_path, socket_path_for_channel, worktree_hash, BuildChannel,
};

/// Get the default log path for the daemon.
pub fn default_log_path() -> PathBuf {
    daemon_base_dir().join("runtimed.log")
}

/// Compare two binary version strings ignoring the optional `+dirty`
/// build-time marker on the SHA. Use this for upgrade/match decisions
/// so a dirty rebuild and a clean rebuild at the same SHA aren't
/// treated as different binaries (which would cause perpetual
/// reinstall loops in dev). Dirty status is informational and surfaced
/// elsewhere; equality should look only at the committed code.
pub fn versions_match_ignoring_dirty(a: &str, b: &str) -> bool {
    strip_dirty_suffix(a) == strip_dirty_suffix(b)
}

fn strip_dirty_suffix(version: &str) -> &str {
    version.strip_suffix("+dirty").unwrap_or(version)
}

#[cfg(test)]
mod version_tests {
    use super::{strip_dirty_suffix, versions_match_ignoring_dirty};

    #[test]
    fn strip_dirty_removes_trailing_marker() {
        assert_eq!(strip_dirty_suffix("2.2.1+abc1234+dirty"), "2.2.1+abc1234");
        assert_eq!(strip_dirty_suffix("2.2.1+abc1234"), "2.2.1+abc1234");
        assert_eq!(strip_dirty_suffix("2.2.1"), "2.2.1");
    }

    #[test]
    fn versions_match_treats_dirty_and_clean_as_same_commit() {
        assert!(versions_match_ignoring_dirty(
            "2.2.1+abc1234",
            "2.2.1+abc1234+dirty"
        ));
        assert!(versions_match_ignoring_dirty(
            "2.2.1+abc1234+dirty",
            "2.2.1+abc1234+dirty"
        ));
        assert!(!versions_match_ignoring_dirty(
            "2.2.1+abc1234",
            "2.2.1+def5678"
        ));
        assert!(!versions_match_ignoring_dirty(
            "2.2.1+abc1234",
            "2.3.0+abc1234"
        ));
    }
}

// ============================================================================
// Types
// ============================================================================

/// Environment types supported by the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvType {
    Uv,
    Conda,
    Pixi,
}

impl std::fmt::Display for EnvType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvType::Uv => write!(f, "uv"),
            EnvType::Conda => write!(f, "conda"),
            EnvType::Pixi => write!(f, "pixi"),
        }
    }
}

/// A prewarmed environment returned by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PooledEnv {
    pub env_type: EnvType,
    pub venv_path: PathBuf,
    pub python_path: PathBuf,
    /// Packages that were pre-installed in this pooled environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prewarmed_packages: Vec<String>,
}

// ============================================================================
// Pool Directory Naming
// ============================================================================

/// Directory name prefixes for prewarmed pool environments.
///
/// Pool envs are created in `default_cache_dir()` with these prefixes
/// followed by a UUID (e.g. `runtimed-uv-a1b2c3d4-...`). GC, room
/// eviction, and pool recovery use these to identify pool-managed dirs
/// vs. content-addressed caches (which are 16-char hex names).
pub const POOL_PREFIX_UV: &str = "runtimed-uv-";
pub const POOL_PREFIX_CONDA: &str = "runtimed-conda-";
pub const POOL_PREFIX_PIXI: &str = "runtimed-pixi-";

/// All pool directory prefixes, for iteration.
pub const POOL_PREFIXES: &[&str] = &[POOL_PREFIX_UV, POOL_PREFIX_CONDA, POOL_PREFIX_PIXI];

/// Check whether a directory name looks like a pool-managed environment.
pub fn is_pool_env_dir(name: &str) -> bool {
    POOL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// Walk up from a pool env's `venv_path` to find the top-level
/// `runtimed-{uv,conda,pixi}-*` directory.
///
/// Pixi envs have nested venv_paths (e.g.
/// `runtimed-pixi-{uuid}/.pixi/envs/default`) but GC and room eviction
/// operate on top-level dirs, so this normalises before comparing or
/// deleting.
///
/// Returns the path unchanged if no pool-prefixed ancestor is found
/// (e.g. for content-addressed envs that are already top-level).
pub fn pool_env_root(path: &Path) -> PathBuf {
    let mut cur = path;
    loop {
        if let Some(name) = cur.file_name().and_then(|n: &OsStr| n.to_str()) {
            if is_pool_env_dir(name) {
                return cur.to_path_buf();
            }
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p,
            _ => break,
        }
    }
    path.to_path_buf()
}

/// Check whether a path is inside the given cache directory.
///
/// Used as a safety gate before `remove_dir_all` to ensure we never
/// accidentally delete paths outside the daemon's cache tree.
pub fn is_within_cache_dir(path: &Path, cache_dir: &Path) -> bool {
    path.starts_with(cache_dir)
}

/// Get the default cache directory for environments.
pub fn default_cache_dir() -> PathBuf {
    daemon_base_dir().join("envs")
}

/// Get the default directory for the content-addressed blob store.
pub fn default_blob_store_dir() -> PathBuf {
    daemon_base_dir().join("blobs")
}

/// Get the default directory for durable execution records.
pub fn default_execution_store_dir() -> PathBuf {
    daemon_base_dir().join("executions")
}

/// Get the directory for kernel connection files.
///
/// Connection files are stored here (rather than the shared Jupyter runtime
/// directory) so the daemon owns its files exclusively. This enables safe
/// bulk cleanup on startup and opens the door for future kernel reattachment
/// during daemon upgrades.
pub fn connections_dir() -> PathBuf {
    daemon_base_dir().join("connections")
}

/// Get the default path for the persisted Automerge settings document.
pub fn default_settings_doc_path() -> PathBuf {
    daemon_base_dir().join("settings.automerge")
}

/// Get the path to the settings JSON Schema file.
pub fn settings_schema_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(config_namespace())
        .join("settings.schema.json")
}

/// Get the default directory for persisted notebook Automerge documents.
pub fn default_notebook_docs_dir() -> PathBuf {
    daemon_base_dir().join("notebook-docs")
}

/// Base directory for kernel IPC (Unix domain) socket files.
///
/// Uses `/tmp/` to stay within the macOS 104-byte `sun_path` limit —
/// paths under `~/.cache/` can exceed it with long usernames. The UID
/// is embedded in the directory name so that different OS users on the
/// same machine get private, non-overlapping socket roots (the daemon
/// singleton lock is per-user, but `/tmp/` is world-writable). The
/// directory is created with mode `0o700` by `ensure_ipc_socket_dir`.
///
/// Layout: `/tmp/{cache_namespace}-{uid}/` (e.g. `/tmp/runt-nightly-1000/`).
/// In dev mode a worktree hash is appended so concurrent worktree
/// daemons never collide.
/// Kernel socket files are `kernel-{id}-ipc-{1..5}` inside this dir.
#[cfg(unix)]
fn ipc_socket_base_dir() -> PathBuf {
    // SAFETY: getuid() is always safe — it's a read-only syscall with no
    // preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp").join(format!("{}-{}", cache_namespace(), uid))
}

#[cfg(unix)]
pub fn ipc_socket_dir() -> PathBuf {
    let base = ipc_socket_base_dir();
    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            return base.join(format!("wt-{}", worktree_hash(&worktree)));
        }
    }
    base
}

#[cfg(unix)]
fn ensure_private_ipc_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    match std::fs::create_dir(dir) {
        Ok(()) => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }

    let metadata = std::fs::symlink_metadata(dir)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "IPC socket path exists but is not a directory: {}",
                dir.display()
            ),
        ));
    }

    // SAFETY: getuid() is always safe — it's a read-only syscall with no
    // preconditions.
    let uid = unsafe { libc::getuid() };
    if metadata.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "IPC socket directory is owned by uid {}, expected {}: {}",
                metadata.uid(),
                uid,
                dir.display()
            ),
        ));
    }

    if metadata.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Create the IPC socket directory with owner-only permissions (`0o700`).
///
/// Call this before writing kernel socket files. The restricted mode
/// prevents other local users from creating or removing sockets in
/// our directory.
#[cfg(unix)]
pub fn ensure_ipc_socket_dir() -> std::io::Result<()> {
    ensure_private_ipc_dir(&ipc_socket_base_dir())?;
    let dir = ipc_socket_dir();
    if dir != ipc_socket_base_dir() {
        ensure_private_ipc_dir(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pool_env_dir() {
        assert!(is_pool_env_dir("runtimed-uv-a1b2c3d4"));
        assert!(is_pool_env_dir("runtimed-conda-a1b2c3d4"));
        assert!(is_pool_env_dir("runtimed-pixi-a1b2c3d4"));
        assert!(!is_pool_env_dir("abcdef0123456789")); // content-addressed
        assert!(!is_pool_env_dir("some-other-dir"));
    }

    #[test]
    fn test_pool_env_root_uv() {
        // UV venv_path IS the top-level dir — returned unchanged
        let path = Path::new("/cache/envs/runtimed-uv-abc123");
        assert_eq!(pool_env_root(path), path);
    }

    #[test]
    fn test_pool_env_root_pixi_nested() {
        // Pixi venv_path is nested — walk up to top-level
        let path = Path::new("/cache/envs/runtimed-pixi-abc123/.pixi/envs/default");
        assert_eq!(
            pool_env_root(path),
            Path::new("/cache/envs/runtimed-pixi-abc123")
        );
    }

    #[test]
    fn test_pool_env_root_content_addressed() {
        // Content-addressed envs have no runtimed-* ancestor — returned unchanged
        let path = Path::new("/cache/envs/abcdef0123456789");
        assert_eq!(pool_env_root(path), path);
    }

    #[test]
    fn test_is_within_cache_dir() {
        let cache = Path::new("/home/user/.cache/runt-nightly/envs");
        assert!(is_within_cache_dir(
            Path::new("/home/user/.cache/runt-nightly/envs/runtimed-uv-abc"),
            cache
        ));
        assert!(!is_within_cache_dir(
            Path::new("/tmp/runtimed-uv-abc"),
            cache
        ));
        assert!(!is_within_cache_dir(Path::new("/home/user/.cache"), cache));
    }
}
