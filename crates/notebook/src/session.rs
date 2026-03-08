//! Session state persistence for restoring windows across app restarts.
//!
//! Saves the list of open windows (with their notebook paths or env_ids) on shutdown,
//! and restores them on startup. Works with the tauri-plugin-window-state for geometry.

use crate::runtime::Runtime;
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

    let path = runtimed::session_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{json}\n")).map_err(|e| e.to_string())?;

    info!(
        "[session] Saved {} windows to {}",
        session.windows.len(),
        path.display()
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
    let path = runtimed::session_state_path();
    if !path.exists() {
        info!("[session] No session file found at {}", path.display());
        return None;
    }

    let contents = match std::fs::read_to_string(&path) {
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
    let path = runtimed::session_state_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
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
