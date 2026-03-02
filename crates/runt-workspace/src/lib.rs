//! Workspace and dev mode utilities for Runt.
//!
//! This crate provides utilities for detecting development mode and managing
//! workspace-specific paths, enabling per-worktree isolation during development.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Development Mode Detection
// ============================================================================

/// Check if development mode is enabled.
///
/// Dev mode enables per-worktree daemon isolation, allowing each git worktree
/// to run its own daemon instance with separate state directories.
///
/// Returns true if:
/// - `RUNTIMED_DEV=1` is set (explicit opt-in), OR
/// - `CONDUCTOR_WORKSPACE_PATH` is set (automatic for Conductor users)
pub fn is_dev_mode() -> bool {
    // Explicit opt-in
    if std::env::var("RUNTIMED_DEV")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return true;
    }
    // Auto-detect Conductor workspace
    std::env::var("CONDUCTOR_WORKSPACE_PATH").is_ok()
}

/// Get the workspace path for dev mode.
///
/// Uses `CONDUCTOR_WORKSPACE_PATH` if available, otherwise detects via git.
pub fn get_workspace_path() -> Option<PathBuf> {
    // Prefer Conductor's workspace path
    if let Ok(path) = std::env::var("CONDUCTOR_WORKSPACE_PATH") {
        return Some(PathBuf::from(path));
    }
    // Fallback to git detection
    detect_worktree_root()
}

/// Get the workspace name for display.
///
/// Uses `CONDUCTOR_WORKSPACE_NAME` if available, otherwise reads from
/// `.context/workspace-description` file in the worktree.
pub fn get_workspace_name() -> Option<String> {
    // Prefer Conductor's workspace name
    if let Ok(name) = std::env::var("CONDUCTOR_WORKSPACE_NAME") {
        return Some(name);
    }
    // Fallback: read .context/workspace-description
    get_workspace_path()
        .and_then(|p| std::fs::read_to_string(p.join(".context/workspace-description")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect the current git worktree root.
///
/// Runs `git rev-parse --show-toplevel` to find the root directory.
fn detect_worktree_root() -> Option<PathBuf> {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.exists())
}

/// Compute a short hash of a worktree path for directory naming.
///
/// Returns the first 12 hex characters of the SHA-256 hash.
pub fn worktree_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hex::encode(&hasher.finalize()[..6]) // 6 bytes = 12 hex chars
}

// ============================================================================
// Directory Paths
// ============================================================================

/// Get the base directory for the current daemon context.
///
/// In dev mode: `~/.cache/runt/worktrees/{hash}/`
/// Otherwise: `~/.cache/runt/`
pub fn daemon_base_dir() -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt");

    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            let hash = worktree_hash(&worktree);
            return base.join("worktrees").join(hash);
        }
    }
    base
}

/// Get the default directory for saving notebooks.
///
/// In dev mode with a workspace path: `{workspace}/notebooks/`
/// Otherwise: `~/notebooks/`
///
/// Creates the directory if it doesn't exist.
pub fn default_notebooks_dir() -> Result<PathBuf, String> {
    let notebooks_dir = if is_dev_mode() {
        if let Some(workspace) = get_workspace_path() {
            workspace.join("notebooks")
        } else {
            home_notebooks_dir()?
        }
    } else {
        home_notebooks_dir()?
    };

    ensure_dir_exists(&notebooks_dir)?;
    Ok(notebooks_dir)
}

/// Get the ~/notebooks directory path.
fn home_notebooks_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join("notebooks"))
}

/// Ensure a directory exists, creating it if necessary.
fn ensure_dir_exists(dir: &Path) -> Result<(), String> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if dir.is_dir() {
                Ok(())
            } else {
                Err(format!("{} exists but is not a directory", dir.display()))
            }
        }
        Err(e) => Err(format!(
            "Failed to create directory {}: {}",
            dir.display(),
            e
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_hash_consistency() {
        let path = Path::new("/some/path");
        let hash1 = worktree_hash(path);
        let hash2 = worktree_hash(path);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 12);
    }

    #[test]
    fn test_worktree_hash_differs() {
        let path1 = Path::new("/path/one");
        let path2 = Path::new("/path/two");
        assert_ne!(worktree_hash(path1), worktree_hash(path2));
    }
}
