//! Centralized path helpers for the `runtimed` crate — socket, cache, env, and
//! notebook-doc paths live here so callers have one module to look at instead
//! of a scatter across `daemon.rs` and `notebook_sync_server.rs`.
//!
//! Most entries are re-exports of helpers that live in upstream crates
//! (`runt-workspace`, `runtimed-client`, `notebook-doc`) — the point is
//! discoverability, not duplication. Genuinely local helpers
//! (e.g. [`snapshot_before_delete`], [`normalize_save_target`]) live here in
//! full.
//!
//! Keep this module behaviour-preserving. New path logic is fine; silently
//! drifting from the upstream helpers is not.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Re-exports from upstream crates — single entry point for runtimed callers.
// ---------------------------------------------------------------------------

/// Default daemon socket path (honours `RUNTIMED_SOCKET_PATH`).
///
/// Re-exported from `runt_workspace`.
pub use runt_workspace::default_socket_path;

/// Default cache directory for environments (`<daemon_base_dir>/envs`).
///
/// Re-exported from `runtimed_client` (originally defined there because
/// `runtimed-client` owns daemon-path conventions shared with the CLI and
/// Python bindings).
pub use runtimed_client::default_cache_dir;

/// Walk up from a pool env's `venv_path` to the top-level
/// `runtimed-{uv,conda,pixi}-*` directory.
///
/// Re-exported from `runtimed_client`.
pub use runtimed_client::pool_env_root;

/// Deterministic filename for a notebook's persisted Automerge doc.
///
/// Re-exported from `notebook_doc` (behind the `persistence` feature).
pub use notebook_doc::notebook_doc_filename;

// ---------------------------------------------------------------------------
// Local helpers — moved from daemon.rs / notebook_sync_server.rs.
// ---------------------------------------------------------------------------

/// Maximum number of snapshots to keep per notebook hash.
const MAX_SNAPSHOTS_PER_NOTEBOOK: usize = 5;

/// Snapshot a persisted automerge doc before deleting it.
///
/// Copies the file to `{docs_dir}/snapshots/{stem}-{millis}.automerge`
/// and prunes old snapshots beyond `MAX_SNAPSHOTS_PER_NOTEBOOK`. If a
/// paired `{stem}.state.automerge` sidecar exists, it gets a matching
/// `{stem}-{millis}.state.automerge` snapshot so offline recovery can
/// still pair outputs with the notebook doc it found.
///
/// Returns `true` if the snapshot was created successfully. The caller
/// should only delete the original file when this returns `true`.
pub(crate) fn snapshot_before_delete(persist_path: &Path, docs_dir: &Path) -> bool {
    let Some(stem) = persist_path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };

    let snapshots_dir = docs_dir.join("snapshots");
    if let Err(e) = std::fs::create_dir_all(&snapshots_dir) {
        warn!(
            "[notebook-sync] Failed to create snapshots dir {:?}: {}",
            snapshots_dir, e
        );
        return false;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let snapshot_name = format!("{}-{}.automerge", stem, timestamp);
    let snapshot_path = snapshots_dir.join(&snapshot_name);

    match std::fs::copy(persist_path, &snapshot_path) {
        Ok(_) => {
            info!(
                "[notebook-sync] Snapshotted persisted doc before refresh: {:?}",
                snapshot_path
            );
        }
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to snapshot {:?}: {}",
                persist_path, e
            );
            return false;
        }
    }

    // Snapshot the state sidecar with the same timestamp so the pair
    // stays consistent. Absence is normal (pre-v3 rooms, ephemeral),
    // so missing is not an error.
    let state_src = state_persist_path_alongside(persist_path);
    if state_src.exists() {
        let state_snapshot = snapshots_dir.join(format!("{}-{}.state.automerge", stem, timestamp));
        if let Err(e) = std::fs::copy(&state_src, &state_snapshot) {
            warn!(
                "[notebook-sync] Failed to snapshot state sidecar {:?}: {}",
                state_src, e
            );
        }
    }

    // Prune old snapshots for this hash (keep most recent MAX_SNAPSHOTS_PER_NOTEBOOK).
    // Count only notebook-doc snapshots; state-doc siblings are pruned in
    // lockstep via their matching timestamp.
    let prefix = format!("{}-", stem);
    let mut snapshots: Vec<_> = std::fs::read_dir(&snapshots_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|name| {
                name.starts_with(&prefix)
                    && name.ends_with(".automerge")
                    && !name.ends_with(".state.automerge")
            })
        })
        .collect();

    if snapshots.len() > MAX_SNAPSHOTS_PER_NOTEBOOK {
        // Sort by filename (which embeds timestamp) — ascending order
        snapshots.sort_by_key(|e| e.file_name());
        for entry in &snapshots[..snapshots.len() - MAX_SNAPSHOTS_PER_NOTEBOOK] {
            let snapshot_path = entry.path();
            let _ = std::fs::remove_file(&snapshot_path);
            // Prune matching state sidecar snapshot if present.
            if let Some(state_sibling) = state_sidecar_for_snapshot(&snapshot_path) {
                let _ = std::fs::remove_file(state_sibling);
            }
        }
    }

    true
}

/// Compute `{stem}.state.automerge` next to a `{stem}.automerge` file.
fn state_persist_path_alongside(notebook_path: &Path) -> PathBuf {
    let parent = notebook_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = notebook_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("doc");
    parent.join(format!("{stem}.state.automerge"))
}

/// Given a snapshot path `{stem}-{ts}.automerge`, compute the matching
/// state sidecar snapshot path `{stem}-{ts}.state.automerge`. Returns
/// `None` when the input doesn't match the expected shape.
fn state_sidecar_for_snapshot(snapshot_path: &Path) -> Option<PathBuf> {
    let parent = snapshot_path.parent()?;
    let stem = snapshot_path.file_stem()?.to_str()?;
    Some(parent.join(format!("{stem}.state.automerge")))
}

/// Normalize a user-supplied save target: append `.ipynb` if missing, reject
/// relative paths, and return the path that `save_notebook_to_disk` will use.
pub(crate) fn normalize_save_target(target: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(target);
    if path.is_relative() {
        return Err(format!(
            "Relative paths are not supported for save: '{}'. Please provide an absolute path.",
            target
        ));
    }
    Ok(if target.ends_with(".ipynb") {
        path
    } else {
        PathBuf::from(format!("{}.ipynb", target))
    })
}

/// Returns true if `path` looks like a bare UUID with no extension and no
/// parent components — the shape daemon-side code produces for untitled
/// notebooks. Used as a guard before treating a path string as a real file.
pub(crate) fn looks_like_untitled_notebook_path(path: &str) -> bool {
    let candidate = Path::new(path);
    candidate.components().count() == 1
        && candidate.extension().is_none()
        && uuid::Uuid::parse_str(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_uuid_looks_untitled() {
        assert!(looks_like_untitled_notebook_path(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!looks_like_untitled_notebook_path(
            "/tmp/550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!looks_like_untitled_notebook_path(
            "550e8400-e29b-41d4-a716-446655440000.ipynb"
        ));
    }

    #[test]
    fn normalize_save_target_appends_extension() {
        let result = normalize_save_target("/tmp/foo").unwrap();
        assert_eq!(result, PathBuf::from("/tmp/foo.ipynb"));
    }

    #[test]
    fn normalize_save_target_preserves_extension() {
        let result = normalize_save_target("/tmp/foo.ipynb").unwrap();
        assert_eq!(result, PathBuf::from("/tmp/foo.ipynb"));
    }

    #[test]
    fn normalize_save_target_rejects_relative() {
        assert!(normalize_save_target("foo.ipynb").is_err());
    }

    /// snapshot_before_delete must mirror the notebook doc snapshot onto
    /// its paired `.state.automerge` sidecar so offline recovery pairs
    /// outputs with the right notebook snapshot.
    #[test]
    fn snapshot_before_delete_preserves_state_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let docs_dir = tmp.path().to_path_buf();

        let persist_path = docs_dir.join("abc.automerge");
        let state_path = docs_dir.join("abc.state.automerge");
        std::fs::write(&persist_path, b"notebook-bytes").unwrap();
        std::fs::write(&state_path, b"state-bytes").unwrap();

        assert!(snapshot_before_delete(&persist_path, &docs_dir));

        let snapshots_dir = docs_dir.join("snapshots");
        let mut notebook_snapshots: Vec<_> = std::fs::read_dir(&snapshots_dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name().to_str().is_some_and(|n| {
                    n.starts_with("abc-")
                        && n.ends_with(".automerge")
                        && !n.ends_with(".state.automerge")
                })
            })
            .collect();
        notebook_snapshots.sort_by_key(|e| e.file_name());
        assert_eq!(notebook_snapshots.len(), 1, "notebook snapshot missing");

        let state_snapshots: Vec<_> = std::fs::read_dir(&snapshots_dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("abc-") && n.ends_with(".state.automerge"))
            })
            .collect();
        assert_eq!(
            state_snapshots.len(),
            1,
            "state sidecar snapshot missing — offline recovery would pair the notebook snapshot with stale outputs"
        );
    }
}
