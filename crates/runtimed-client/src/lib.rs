//! Client library for communicating with the runtimed daemon.
//!
//! This crate provides the client-facing API for runtimed: IPC client,
//! settings sync, daemon discovery, service management, and shared types.
//! It does NOT include any server-side daemon code or heavy dependencies
//! like rattler, kernel-env, or kernel-launch.
//!
//! ## Crate consumers
//!
//! - `notebook` (Tauri app) — daemon lifecycle, settings sync
//! - `runt` (CLI) — daemon management commands
//! - `runtimed-py` (Python bindings) — PoolClient, SyncClient, settings
//! - `runtimed` (daemon) — re-exports everything, adds server-only code

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod client;
pub mod daemon_paths;
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

/// Get the default cache directory for environments.
pub fn default_cache_dir() -> PathBuf {
    daemon_base_dir().join("envs")
}

/// Get the default directory for the content-addressed blob store.
pub fn default_blob_store_dir() -> PathBuf {
    daemon_base_dir().join("blobs")
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
