//! Minimal project file detection for MCP tools.
//!
//! Walks up from a start path looking for `pyproject.toml` or `pixi.toml`.
//! Used to determine whether dep management should target a project file
//! (via `pixi add` / `uv add`) or notebook inline metadata.

use std::path::{Path, PathBuf};

/// The type of project file detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFileKind {
    PyprojectToml,
    PixiToml,
}

/// A detected project file with its path and kind.
#[derive(Debug, Clone)]
pub struct DetectedProjectFile {
    pub path: PathBuf,
    pub kind: ProjectFileKind,
}

impl DetectedProjectFile {
    /// The package manager for this project file.
    pub fn manager(&self) -> notebook_protocol::connection::PackageManager {
        use notebook_protocol::connection::PackageManager;
        match self.kind {
            ProjectFileKind::PyprojectToml => PackageManager::Uv,
            ProjectFileKind::PixiToml => PackageManager::Pixi,
        }
    }

    /// The daemon env_source for this project type.
    pub fn env_source(&self) -> notebook_protocol::connection::EnvSource {
        use notebook_protocol::connection::EnvSource;
        match self.kind {
            ProjectFileKind::PyprojectToml => EnvSource::Pyproject,
            ProjectFileKind::PixiToml => EnvSource::PixiToml,
        }
    }
}

/// Detect the nearest project file by walking up from `start_path`.
///
/// Checks each directory for `pyproject.toml` and `pixi.toml`.
/// A `pyproject.toml` with `[tool.pixi]` is treated as a pixi project.
/// Stops at `.git` boundaries or the user's home directory.
pub fn detect_project_file(start_path: &Path) -> Option<DetectedProjectFile> {
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();
    let mut current = start_dir.to_path_buf();

    loop {
        // Check pyproject.toml first (higher priority in tiebreaker)
        let pyproject = current.join("pyproject.toml");
        if pyproject.exists() {
            // If it has [tool.pixi], treat as pixi project
            if has_pixi_section(&pyproject) {
                return Some(DetectedProjectFile {
                    path: pyproject,
                    kind: ProjectFileKind::PixiToml,
                });
            }
            return Some(DetectedProjectFile {
                path: pyproject,
                kind: ProjectFileKind::PyprojectToml,
            });
        }

        // Check pixi.toml
        let pixi = current.join("pixi.toml");
        if pixi.exists() {
            return Some(DetectedProjectFile {
                path: pixi,
                kind: ProjectFileKind::PixiToml,
            });
        }

        // Stop at home directory or git repo root
        if let Some(ref home) = home_dir {
            if current == *home {
                return None;
            }
        }
        if current.join(".git").exists() {
            return None;
        }

        match current.parent() {
            Some(parent) if parent != current => {
                current = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

/// Check if a pyproject.toml contains a `[tool.pixi]` section.
fn has_pixi_section(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|content| content.contains("[tool.pixi]") || content.contains("[tool.pixi."))
}
