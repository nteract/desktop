//! runtimed - Central daemon for managing Jupyter runtimes and prewarmed environments.
//!
//! This crate provides a daemon process that manages a shared pool of prewarmed
//! Python environments (UV and Conda), a content-addressed blob store for
//! notebook outputs, and an Automerge-based settings sync service.
//!
//! ## Protocol types
//!
//! The wire protocol types (`connection`, `protocol`) are defined in the
//! `notebook-protocol` crate and re-exported here for backward compatibility.
//! New code should prefer importing from `notebook_protocol` directly.
//!
//! All services communicate over a single Unix socket (named pipe on Windows)
//! using length-prefixed binary framing with a channel-based handshake.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod blob_server;
pub mod blob_store;
pub mod client;
pub mod comm_state;
/// Re-export connection types from notebook-protocol.
///
/// The canonical definitions live in `notebook_protocol::connection`.
/// This re-export preserves backward compatibility for existing callers.
pub use notebook_protocol::connection;
pub mod daemon;
pub mod inline_env;
pub mod kernel_manager;
pub mod markdown_assets;
pub use notebook_doc;
pub use notebook_doc::metadata as notebook_metadata;
pub mod notebook_sync_client;
pub mod notebook_sync_server;
pub mod output_store;
pub mod project_file;
pub mod protocol;
pub mod runtime;
pub mod service;
pub mod settings_doc;
pub mod singleton;
pub mod stream_terminal;
pub mod sync_client;
pub mod sync_server;
pub mod terminal_size;

// ============================================================================
// Development Mode and Worktree Isolation
// ============================================================================

// Re-export from runt-workspace for backwards compatibility
pub use runt_workspace::{
    build_channel, cache_namespace, config_namespace, daemon_base_dir, daemon_binary_basename,
    daemon_launchd_label, daemon_service_basename, desktop_display_name, desktop_product_name,
    get_workspace_name, get_workspace_path, is_dev_mode, worktree_hash,
};

/// Get the default log path for the daemon.
pub fn default_log_path() -> PathBuf {
    daemon_base_dir().join("runtimed.log")
}

/// Get the daemon version string (e.g., "0.1.0-dev.10+abc123").
/// Used for protocol version checking and debugging.
/// Cached to avoid repeated allocations on hot paths.
pub fn daemon_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("GIT_COMMIT")))
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
}

impl std::fmt::Display for EnvType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvType::Uv => write!(f, "uv"),
            EnvType::Conda => write!(f, "conda"),
        }
    }
}

/// A prewarmed environment returned by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PooledEnv {
    pub env_type: EnvType,
    pub venv_path: PathBuf,
    pub python_path: PathBuf,
}

/// Pool statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub uv_available: usize,
    pub uv_warming: usize,
    pub uv_target: usize,
    pub conda_available: usize,
    pub conda_warming: usize,
    pub conda_target: usize,
    /// Error info for UV pool (if warming is failing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_error: Option<PoolError>,
    /// Error info for Conda pool (if warming is failing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conda_error: Option<PoolError>,
}

/// Error information for a pool that is failing to warm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolError {
    /// Human-readable error message.
    pub message: String,
    /// Package that failed to install (if identified).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_package: Option<String>,
    /// Number of consecutive failures.
    pub consecutive_failures: u32,
    /// Seconds until next retry (0 if retry is imminent).
    pub retry_in_secs: u64,
}

/// Get the default endpoint path for runtimed.
///
/// On Unix, this returns a Unix socket path (e.g., ~/.cache/runt*/runtimed.sock).
/// In dev mode, returns the per-worktree socket path.
/// On Windows, this returns a named pipe path (e.g., \\.\pipe\runtimed).
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("RUNTIMED_SOCKET_PATH") {
        let p = p.trim();
        if !p.is_empty() {
            let path = PathBuf::from(p);
            // Validate parent directory exists (socket file may not exist yet)
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    return path;
                }
                panic!(
                    "RUNTIMED_SOCKET_PATH directory does not exist: {}",
                    parent.display()
                );
            }
        }
    }
    daemon_base_dir().join("runtimed.sock")
}

/// Get the default endpoint path for runtimed.
///
/// On Unix, this returns a Unix socket path (e.g., ~/.cache/runt*/runtimed.sock).
/// On Windows, this returns a named pipe path (e.g., \\.\pipe\runtimed).
/// In dev mode on Windows, appends the worktree hash to the pipe name.
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("RUNTIMED_SOCKET_PATH") {
        let p = p.trim();
        if !p.is_empty() {
            let path = PathBuf::from(p);
            // Validate parent directory exists (socket file may not exist yet)
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    return path;
                }
                panic!(
                    "RUNTIMED_SOCKET_PATH directory does not exist: {}",
                    parent.display()
                );
            }
        }
    }
    let pipe_name = daemon_binary_basename();
    // Windows named pipes use the \\.\pipe\name format
    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            let hash = worktree_hash(&worktree);
            return PathBuf::from(format!(r"\\.\pipe\{}-{}", pipe_name, hash));
        }
    }
    PathBuf::from(format!(r"\\.\pipe\{}", pipe_name))
}

/// Get the default cache directory for environments.
pub fn default_cache_dir() -> PathBuf {
    daemon_base_dir().join("envs")
}

/// Get the default directory for the content-addressed blob store.
pub fn default_blob_store_dir() -> PathBuf {
    daemon_base_dir().join("blobs")
}

/// Get the default path for the persisted Automerge settings document.
pub fn default_settings_doc_path() -> PathBuf {
    daemon_base_dir().join("settings.automerge")
}

/// Get the path to the JSON settings file (for migration and fallback).
pub fn settings_json_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(config_namespace())
        .join("settings.json")
}

/// Get the path to the session state file.
///
/// In dev mode: stored per-worktree for isolation during development.
/// In production: stored in config directory alongside settings.
pub fn session_state_path() -> PathBuf {
    if is_dev_mode() {
        // Per-worktree session for dev isolation
        daemon_base_dir().join("session.json")
    } else {
        // Production: config directory
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(config_namespace())
            .join("session.json")
    }
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
