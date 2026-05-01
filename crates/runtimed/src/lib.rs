//! runtimed - Central daemon for managing Jupyter runtimes and prewarmed environments.
//!
//! This crate provides a daemon process that manages a shared pool of prewarmed
//! Python environments (UV and Conda), a content-addressed blob store for
//! notebook outputs, and an Automerge-based settings sync service.
//!
//! Client-facing types and APIs (PoolClient, SyncClient, settings, singleton
//! discovery, service management) live in the `runtimed-client` crate and are
//! re-exported here for backward compatibility.
//!
//! All services communicate over a single Unix socket (named pipe on Windows)
//! using length-prefixed binary framing with a channel-based handshake.

// Re-export everything from runtimed-client for backward compatibility
// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use runtimed_client::*;

// ============================================================================
// Server-only modules (not in runtimed-client)
// ============================================================================

pub mod blob_server;
pub mod blob_store;
pub mod daemon;
pub mod daemon_telemetry;
pub mod dx_blob_comm;
pub mod embedded_plugins;
pub mod inline_env;
pub(crate) mod ipykernel_error;
pub mod jupyter_kernel;
pub mod kernel_connection;
pub(crate) mod kernel_ports;
pub mod kernel_state;
pub mod launcher_cache;
pub mod markdown_assets;
pub mod notebook_sync_server;
pub mod output_prep;
pub mod output_store;
pub mod paths;
pub mod process_groups;
pub mod project_file;
pub(crate) mod requests;
pub mod runtime_agent;
pub mod runtime_agent_handle;
pub(crate) mod runtime_agent_manifest;
pub mod singleton;
pub mod stream_terminal;
pub mod sync_server;
pub mod task_supervisor;
pub mod terminal_size;
pub(crate) mod trusted_packages;
pub mod user_error;

pub fn trusted_packages_db_path() -> std::path::PathBuf {
    runtimed_client::daemon_base_dir().join("trusted-packages.sqlite")
}

/// Get the daemon version string (e.g., "0.1.0-dev.10+abc123").
/// Used for protocol version checking and debugging.
/// Cached to avoid repeated allocations on hot paths.
pub fn daemon_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        format!(
            "{}+{}",
            env!("CARGO_PKG_VERSION"),
            include_str!(concat!(env!("OUT_DIR"), "/git_hash.txt"))
        )
    })
}
