//! Project file detection with "closest wins" semantics.
//!
//! Walks up from the notebook directory, checking for project files at each
//! level. The first (closest) match wins, with tiebreaker priority when
//! multiple files exist at the same level.
//!
//! This is adapted from `crates/notebook/src/project_file.rs` for use in
//! the daemon's environment auto-detection.

use std::path::{Path, PathBuf};

/// The type of project file detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFileKind {
    PyprojectToml,
    PixiToml,
    EnvironmentYml,
}

/// A detected project file with its path and kind.
#[derive(Debug, Clone)]
pub struct DetectedProjectFile {
    pub path: PathBuf,
    pub kind: ProjectFileKind,
}

impl DetectedProjectFile {
    /// Convert to env_source string for kernel launch.
    pub fn to_env_source(&self) -> &'static str {
        match self.kind {
            ProjectFileKind::PyprojectToml => "uv:pyproject",
            ProjectFileKind::PixiToml => "pixi:toml",
            ProjectFileKind::EnvironmentYml => "conda:env_yml",
        }
    }
}

/// Mapping from filename to project file kind, in tiebreaker priority order.
const ALL_CANDIDATES: &[(&str, ProjectFileKind)] = &[
    ("pyproject.toml", ProjectFileKind::PyprojectToml),
    ("pixi.toml", ProjectFileKind::PixiToml),
    ("environment.yml", ProjectFileKind::EnvironmentYml),
    ("environment.yaml", ProjectFileKind::EnvironmentYml),
];

/// Walk up from `start_path` checking each directory for project files.
///
/// Returns the first (closest) match. Within a single directory, tiebreaker
/// order is: pyproject.toml > pixi.toml > environment.yml > environment.yaml.
///
/// The `kinds` parameter controls which file types to search for. Pass a subset
/// to exclude types that can't be used (e.g., omit `PyprojectToml` when uv is
/// not available so the search continues to find pixi or environment.yml).
///
/// Stops at home directory or `.git` boundary.
pub fn find_nearest_project_file(
    start_path: &Path,
    kinds: &[ProjectFileKind],
) -> Option<DetectedProjectFile> {
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();

    let mut current = start_dir.to_path_buf();
    loop {
        // Check all requested project file types at this level, in tiebreaker order
        for (filename, kind) in ALL_CANDIDATES {
            if !kinds.contains(kind) {
                continue;
            }
            let candidate = current.join(filename);
            if candidate.exists() {
                return Some(DetectedProjectFile {
                    path: candidate,
                    kind: kind.clone(),
                });
            }
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

        // Move to parent directory
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent.to_path_buf();
            }
            _ => return None, // Reached filesystem root
        }
    }
}

/// Convenience function: detect project file with all kinds enabled.
pub fn detect_project_file(notebook_path: &Path) -> Option<DetectedProjectFile> {
    let all_kinds = vec![
        ProjectFileKind::PyprojectToml,
        ProjectFileKind::PixiToml,
        ProjectFileKind::EnvironmentYml,
    ];
    find_nearest_project_file(notebook_path, &all_kinds)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn test_closest_wins_pixi_over_distant_pyproject() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        let notebooks = project.join("notebooks");
        std::fs::create_dir_all(&notebooks).unwrap();

        write_file(&project, "pyproject.toml", "[project]\nname = \"test\"");
        write_file(&notebooks, "pixi.toml", "[project]\nname = \"test\"");

        let found = detect_project_file(&notebooks);
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::PixiToml);
        assert_eq!(found.to_env_source(), "pixi:toml");
    }

    #[test]
    fn test_no_project_files() {
        let temp = TempDir::new().unwrap();
        let found = detect_project_file(temp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_pyproject_env_source() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "pyproject.toml", "[project]\nname = \"test\"");

        let found = detect_project_file(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap().to_env_source(), "uv:pyproject");
    }

    #[test]
    fn test_environment_yml_env_source() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "environment.yml", "name: test");

        let found = detect_project_file(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap().to_env_source(), "conda:env_yml");
    }
}
