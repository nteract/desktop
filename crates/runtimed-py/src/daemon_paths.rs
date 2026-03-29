//! Shared helpers for daemon socket and blob store paths.

use std::path::{Path, PathBuf};

/// Resolve a notebook identifier to a canonical path when it looks like a file path.
///
/// UUIDs and opaque identifiers pass through unchanged. Relative paths like
/// `"notebook.ipynb"` are resolved against the current working directory so
/// they match the canonical keys the daemon uses for notebook rooms.
pub fn resolve_notebook_path(notebook_id: &str) -> String {
    if uuid::Uuid::parse_str(notebook_id).is_ok() {
        return notebook_id.to_string();
    }
    let path = Path::new(notebook_id);
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical.to_string_lossy().to_string();
    }
    if let Ok(absolute) = std::path::absolute(path) {
        return absolute.to_string_lossy().to_string();
    }
    notebook_id.to_string()
}

/// Get the daemon socket path, respecting RUNTIMED_SOCKET_PATH env var.
pub fn get_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("RUNTIMED_SOCKET_PATH") {
        PathBuf::from(p)
    } else {
        runt_workspace::default_socket_path()
    }
}

/// Resolve blob server URL and blob store path from the daemon directory (sync).
///
/// Returns (blob_base_url, blob_store_path).
#[allow(dead_code)] // Public API surface — not yet called from Python bindings
pub fn get_blob_paths_sync(socket_path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Some(parent) = socket_path.parent() else {
        return (None, None);
    };

    let daemon_json = parent.join("daemon.json");
    let base_url = if daemon_json.exists() {
        std::fs::read_to_string(&daemon_json)
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
            .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
            .map(|port| format!("http://localhost:{}", port))
    } else {
        None
    };

    let store_path = parent.join("blobs");
    let store_path = if store_path.exists() {
        Some(store_path)
    } else {
        None
    };

    (base_url, store_path)
}

/// Resolve blob server URL and blob store path from the daemon directory (async).
///
/// Returns (blob_base_url, blob_store_path).
#[allow(dead_code)] // Public API surface — not yet called from Python bindings
pub async fn get_blob_paths_async(socket_path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Some(parent) = socket_path.parent() else {
        return (None, None);
    };

    let daemon_json = parent.join("daemon.json");
    let base_url = if daemon_json.exists() {
        tokio::fs::read_to_string(&daemon_json)
            .await
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
            .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
            .map(|port| format!("http://localhost:{}", port))
    } else {
        None
    };

    let store_path = parent.join("blobs");
    let store_path = if store_path.exists() {
        Some(store_path)
    } else {
        None
    };

    (base_url, store_path)
}
