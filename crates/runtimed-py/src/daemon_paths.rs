//! Shared helpers for daemon socket and blob store paths.

use std::path::{Path, PathBuf};

/// Get the daemon socket path, respecting RUNTIMED_SOCKET_PATH env var.
pub fn get_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("RUNTIMED_SOCKET_PATH") {
        PathBuf::from(p)
    } else {
        runtimed::default_socket_path()
    }
}

/// Resolve blob server URL and blob store path from the daemon directory (sync).
///
/// Returns (blob_base_url, blob_store_path).
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
            .map(|port| format!("http://127.0.0.1:{}", port))
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
            .map(|port| format!("http://127.0.0.1:{}", port))
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
