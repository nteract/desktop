//! Session state persistence for restoring windows across app restarts.
//!
//! Saves the list of open windows (with their notebook paths or env_ids) on shutdown,
//! and restores them on startup. Works with the tauri-plugin-window-state for geometry.

use crate::WindowNotebookRegistry;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a single window's session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSession {
    /// Window label (e.g., "main", "notebook-{hash}")
    pub label: String,
    /// File path for saved notebooks, None for untitled
    pub path: Option<PathBuf>,
    /// env_id from notebook metadata for untitled notebooks.
    /// This allows the daemon to restore the correct Automerge doc.
    pub env_id: Option<String>,
    /// Runtime type (python, deno)
    pub runtime: String,
}

/// Complete application session state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    /// Schema version for forward compatibility
    pub schema_version: u32,
    /// ISO 8601 timestamp when session was saved
    pub saved_at: String,
    /// List of open windows
    pub windows: Vec<WindowSession>,
}

impl SessionState {
    /// Current schema version
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    /// Maximum age in hours before a session is considered stale
    pub const MAX_AGE_HOURS: i64 = 24;
}

/// Save the current session state to disk.
pub(crate) fn save_session(registry: &WindowNotebookRegistry) -> Result<(), String> {
    save_session_to(registry, &runtimed::session_state_path())
}

/// Save the current session state to a specific path.
pub(crate) fn save_session_to(
    registry: &WindowNotebookRegistry,
    dest: &std::path::Path,
) -> Result<(), String> {
    let contexts = registry.contexts.lock().map_err(|e| e.to_string())?;

    let windows: Vec<WindowSession> = contexts
        .iter()
        .filter_map(|(label, context)| {
            let path = context.path.lock().ok()?.clone();
            let notebook_id = context.notebook_id.lock().ok()?.clone();

            // For untitled notebooks (no path), the notebook_id is the env_id (UUID).
            // The daemon uses this to find the persisted Automerge doc on restore.
            let env_id = if path.is_none() && !notebook_id.is_empty() {
                Some(notebook_id)
            } else {
                None
            };

            Some(WindowSession {
                label: label.clone(),
                path,
                env_id,
                runtime: context.runtime.to_string(),
            })
        })
        .collect();

    if windows.is_empty() {
        info!("[session] No windows to save");
        return Ok(());
    }

    let session = SessionState {
        schema_version: SessionState::CURRENT_SCHEMA_VERSION,
        saved_at: chrono::Utc::now().to_rfc3339(),
        windows,
    };

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
    std::fs::write(dest, format!("{json}\n")).map_err(|e| e.to_string())?;

    info!(
        "[session] Saved {} windows to {}",
        session.windows.len(),
        dest.display()
    );
    Ok(())
}

/// Load session state from disk.
///
/// Returns None if:
/// - Session file doesn't exist
/// - Session is too old (> 24 hours)
/// - Session file is corrupted
pub fn load_session() -> Option<SessionState> {
    load_session_from(&runtimed::session_state_path())
}

/// Load session state from a specific path.
pub(crate) fn load_session_from(path: &std::path::Path) -> Option<SessionState> {
    if !path.exists() {
        info!("[session] No session file found at {}", path.display());
        return None;
    }

    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("[session] Failed to read session file: {}", e);
            return None;
        }
    };

    let session: SessionState = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(e) => {
            warn!("[session] Failed to parse session file: {}", e);
            return None;
        }
    };

    // Check session age using seconds for precision
    if let Ok(saved_at) = chrono::DateTime::parse_from_rfc3339(&session.saved_at) {
        let age = chrono::Utc::now().signed_duration_since(saved_at);
        let max_age_seconds = SessionState::MAX_AGE_HOURS * 3600;
        if age.num_seconds() > max_age_seconds {
            let hours = age.num_seconds() / 3600;
            info!("[session] Session too old ({}h), skipping restore", hours);
            return None;
        }
    }

    info!(
        "[session] Loaded session with {} windows",
        session.windows.len()
    );
    Some(session)
}

/// Delete the session file after successful restore.
pub fn clear_session() {
    clear_session_at(&runtimed::session_state_path());
}

/// Delete a specific session file.
pub(crate) fn clear_session_at(path: &std::path::Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!("[session] Failed to remove session file: {}", e);
        } else {
            info!("[session] Cleared session file");
        }
    }
}

/// Generate a stable window label from a session entry.
///
/// Uses deterministic labels so window-state plugin can restore geometry.
pub fn window_label_for_session(session: &WindowSession) -> String {
    if session.label == "main" {
        return "main".to_string();
    }

    if let Some(path) = &session.path {
        // Hash the path for a stable label
        let hash = runtimed::worktree_hash(path);
        format!("notebook-{}", &hash[..8])
    } else if let Some(env_id) = &session.env_id {
        // Use env_id prefix for untitled notebooks
        format!("notebook-{}", &env_id[..8.min(env_id.len())])
    } else {
        // Fallback to UUID
        format!("notebook-{}", uuid::Uuid::new_v4())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::WindowNotebookContext;
    use runtimed::runtime::Runtime;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    fn test_context(path: Option<PathBuf>, notebook_id: &str) -> WindowNotebookContext {
        WindowNotebookContext {
            notebook_sync: Arc::new(tokio::sync::Mutex::new(None)),
            sync_generation: Arc::new(AtomicU64::new(0)),
            path: Arc::new(Mutex::new(path)),
            working_dir: None,
            dirty: Arc::new(AtomicBool::new(false)),
            notebook_id: Arc::new(Mutex::new(notebook_id.to_string())),
            runtime: Runtime::Python,
        }
    }

    fn test_registry(entries: Vec<(&str, WindowNotebookContext)>) -> crate::WindowNotebookRegistry {
        let registry = crate::WindowNotebookRegistry::default();
        {
            let mut contexts = registry.contexts.lock().unwrap();
            for (label, ctx) in entries {
                contexts.insert(label.to_string(), ctx);
            }
        }
        registry
    }

    #[test]
    fn test_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");

        let saved_path = dir.path().join("my_notebook.ipynb");
        std::fs::write(&saved_path, "{}").unwrap();

        let registry = test_registry(vec![
            ("main", test_context(Some(saved_path.clone()), "")),
            ("notebook-abc12345", test_context(None, "env-uuid-1234")),
        ]);

        save_session_to(&registry, &session_path).unwrap();
        assert!(session_path.exists());

        let loaded = load_session_from(&session_path).unwrap();
        assert_eq!(loaded.schema_version, SessionState::CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.windows.len(), 2);

        let main_win = loaded.windows.iter().find(|w| w.label == "main").unwrap();
        assert_eq!(main_win.path.as_ref().unwrap(), &saved_path);
        assert!(main_win.env_id.is_none());

        let untitled = loaded
            .windows
            .iter()
            .find(|w| w.label == "notebook-abc12345")
            .unwrap();
        assert!(untitled.path.is_none());
        assert_eq!(untitled.env_id.as_deref().unwrap(), "env-uuid-1234");
        assert_eq!(untitled.runtime, "python");
    }

    #[test]
    fn test_save_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");

        let registry = test_registry(vec![]);
        save_session_to(&registry, &session_path).unwrap();

        // Empty registry should not create a session file
        assert!(!session_path.exists());
    }

    #[test]
    fn test_load_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("nonexistent.json");
        assert!(load_session_from(&session_path).is_none());
    }

    #[test]
    fn test_load_corrupted_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");
        std::fs::write(&session_path, "not valid json {{{").unwrap();
        assert!(load_session_from(&session_path).is_none());
    }

    #[test]
    fn test_load_stale_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");

        let stale_time =
            chrono::Utc::now() - chrono::Duration::hours(SessionState::MAX_AGE_HOURS + 1);
        let session = SessionState {
            schema_version: SessionState::CURRENT_SCHEMA_VERSION,
            saved_at: stale_time.to_rfc3339(),
            windows: vec![WindowSession {
                label: "main".to_string(),
                path: None,
                env_id: Some("test".to_string()),
                runtime: "python".to_string(),
            }],
        };
        let json = serde_json::to_string_pretty(&session).unwrap();
        std::fs::write(&session_path, format!("{json}\n")).unwrap();

        assert!(load_session_from(&session_path).is_none());
    }

    #[test]
    fn test_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");

        let registry = test_registry(vec![("main", test_context(None, "env-id"))]);
        save_session_to(&registry, &session_path).unwrap();
        assert!(session_path.exists());

        clear_session_at(&session_path);
        assert!(!session_path.exists());
    }

    #[test]
    fn test_window_label_determinism() {
        let session = WindowSession {
            label: "notebook-12345678".to_string(),
            path: Some(PathBuf::from("/tmp/test.ipynb")),
            env_id: None,
            runtime: "python".to_string(),
        };

        let label1 = window_label_for_session(&session);
        let label2 = window_label_for_session(&session);
        assert_eq!(label1, label2);
        assert!(label1.starts_with("notebook-"));
    }

    #[test]
    fn test_window_label_main_passthrough() {
        let session = WindowSession {
            label: "main".to_string(),
            path: None,
            env_id: None,
            runtime: "python".to_string(),
        };
        assert_eq!(window_label_for_session(&session), "main");
    }

    #[test]
    fn test_window_label_untitled_uses_env_id() {
        let session = WindowSession {
            label: "notebook-old".to_string(),
            path: None,
            env_id: Some("abcdef1234567890".to_string()),
            runtime: "python".to_string(),
        };
        assert_eq!(window_label_for_session(&session), "notebook-abcdef12");
    }

    /// Regression test for #848: ghost entries from destroyed windows must be
    /// pruned before saving the session. Before fix #883, stale entries
    /// persisted in the registry and corrupted the session file, causing only
    /// an Untitled notebook to load after an update restart.
    #[test]
    fn test_prune_removes_ghost_entries_before_save() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");

        let nb_path = dir.path().join("real.ipynb");
        std::fs::write(&nb_path, "{}").unwrap();

        // Populate registry with 3 entries: 2 real windows + 1 ghost
        let registry = test_registry(vec![
            ("main", test_context(Some(nb_path.clone()), "")),
            ("notebook-real", test_context(None, "env-uuid-5678")),
            ("notebook-ghost", test_context(None, "env-ghost-dead")),
        ]);

        // Before pruning, all 3 entries are in the registry
        assert_eq!(registry.contexts.lock().unwrap().len(), 3);

        // Prune: simulate that "notebook-ghost" window no longer exists
        // (only "main" and "notebook-real" are live windows)
        registry.prune_where(|label| label == "notebook-ghost");

        // Ghost entry is gone from the registry
        assert_eq!(registry.contexts.lock().unwrap().len(), 2);
        assert!(!registry
            .contexts
            .lock()
            .unwrap()
            .contains_key("notebook-ghost"));

        // Save after pruning — session must only contain the 2 live windows
        save_session_to(&registry, &session_path).unwrap();
        let loaded = load_session_from(&session_path).unwrap();

        assert_eq!(loaded.windows.len(), 2);
        let labels: Vec<&str> = loaded.windows.iter().map(|w| w.label.as_str()).collect();
        assert!(labels.contains(&"main"));
        assert!(labels.contains(&"notebook-real"));
        assert!(!labels.contains(&"notebook-ghost"));
    }
}
