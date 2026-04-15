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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── resolve_notebook_path ─────────────────────────────────────────

    #[test]
    fn resolve_uuid_passes_through_unchanged() {
        // UUIDs are the canonical key for ephemeral / untitled notebooks —
        // they must round-trip without any path canonicalization happening.
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(resolve_notebook_path(id), id);
    }

    #[test]
    fn resolve_uuid_lowercase_and_uppercase_both_pass_through() {
        // Both cases parse as Uuid.
        let lower = "0a1b2c3d-4e5f-6789-abcd-ef0123456789";
        let upper = "0A1B2C3D-4E5F-6789-ABCD-EF0123456789";
        assert_eq!(resolve_notebook_path(lower), lower);
        assert_eq!(resolve_notebook_path(upper), upper);
    }

    #[test]
    fn resolve_existing_file_canonicalizes_to_absolute_path() {
        let tmp = TempDir::new().unwrap();
        let nb = tmp.path().join("nb.ipynb");
        fs::write(&nb, "{}").unwrap();

        let resolved = resolve_notebook_path(nb.to_str().unwrap());
        // canonical path should be absolute and end with the file name.
        let resolved_path = Path::new(&resolved);
        assert!(
            resolved_path.is_absolute(),
            "expected absolute, got {resolved}"
        );
        assert!(resolved.ends_with("nb.ipynb"), "got {resolved}");
    }

    #[test]
    fn resolve_relative_nonexistent_path_falls_back_to_absolute() {
        // Relative path that doesn't exist on disk → canonicalize fails →
        // std::path::absolute fallback fires (relative to cwd).
        let resolved = resolve_notebook_path("./does/not/exist.ipynb");
        let resolved_path = Path::new(&resolved);
        assert!(
            resolved_path.is_absolute(),
            "absolute fallback should have produced an absolute path, got {resolved}"
        );
        assert!(resolved.ends_with("exist.ipynb"), "got {resolved}");
    }

    #[test]
    fn resolve_opaque_non_uuid_non_path_string_returns_input() {
        // Strings that aren't UUIDs and don't look like real paths
        // can still be passed through (they may be daemon-internal IDs
        // or pre-resolved keys). std::path::absolute may still produce
        // an absolute representation, so we just check it doesn't panic
        // and returns *something*.
        let result = resolve_notebook_path("some-arbitrary-id");
        assert!(!result.is_empty());
    }

    // ── get_blob_paths_sync ───────────────────────────────────────────

    #[test]
    fn blob_paths_sync_returns_none_for_orphan_socket_path() {
        // socket_path with no parent (e.g. "/" or "") → both None.
        let (url, store) = get_blob_paths_sync(Path::new("/"));
        assert!(url.is_none(), "no parent → no daemon.json → no url");
        // "/" exists, /blobs may or may not — assert nothing about store.
        let _ = store;
    }

    #[test]
    fn blob_paths_sync_reads_blob_port_from_daemon_json() {
        let tmp = TempDir::new().unwrap();
        // socket_path's parent dir is what we read from.
        let sock = tmp.path().join("runtimed.sock");
        fs::write(
            tmp.path().join("daemon.json"),
            r#"{"blob_port": 49152, "version": "2.2.0"}"#,
        )
        .unwrap();
        fs::create_dir(tmp.path().join("blobs")).unwrap();

        let (url, store) = get_blob_paths_sync(&sock);
        assert_eq!(url.as_deref(), Some("http://localhost:49152"));
        assert_eq!(store, Some(tmp.path().join("blobs")));
    }

    #[test]
    fn blob_paths_sync_returns_url_none_when_daemon_json_lacks_blob_port() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("runtimed.sock");
        fs::write(tmp.path().join("daemon.json"), r#"{"version":"2.2.0"}"#).unwrap();

        let (url, _) = get_blob_paths_sync(&sock);
        assert!(url.is_none(), "missing blob_port → no url");
    }

    #[test]
    fn blob_paths_sync_returns_url_none_when_daemon_json_is_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("runtimed.sock");
        fs::write(tmp.path().join("daemon.json"), "not json {{").unwrap();

        let (url, _) = get_blob_paths_sync(&sock);
        assert!(url.is_none(), "invalid daemon.json → no url");
    }

    #[test]
    fn blob_paths_sync_returns_store_none_when_blobs_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("runtimed.sock");
        fs::write(tmp.path().join("daemon.json"), r#"{"blob_port":1}"#).unwrap();
        // no blobs/ dir created

        let (_, store) = get_blob_paths_sync(&sock);
        assert!(store.is_none(), "no blobs/ dir → no store path");
    }

    // ── get_blob_paths_async ──────────────────────────────────────────

    #[tokio::test]
    async fn blob_paths_async_matches_sync_for_happy_path() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("runtimed.sock");
        fs::write(tmp.path().join("daemon.json"), r#"{"blob_port": 49200}"#).unwrap();
        fs::create_dir(tmp.path().join("blobs")).unwrap();

        let (sync_url, sync_store) = get_blob_paths_sync(&sock);
        let (async_url, async_store) = get_blob_paths_async(&sock).await;
        assert_eq!(sync_url, async_url);
        assert_eq!(sync_store, async_store);
        assert_eq!(async_url.as_deref(), Some("http://localhost:49200"));
    }
}
