//! Unified project file detection with "closest wins" semantics.
//!
//! Instead of checking for each project file type independently (which lets a
//! distant pyproject.toml beat a nearby pixi.toml), this module does a single
//! walk-up from the notebook directory, checking for ALL project file types at
//! each level. The first (closest) match wins, with a tiebreaker order when
//! multiple files exist at the same level.

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
/// Stops at home directory or `.git` boundary (same rules as the individual
/// `find_*` functions).
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

        // pyproject.toml at project root (farther)
        write_file(&project, "pyproject.toml", "[project]\nname = \"test\"");
        // pixi.toml next to notebooks (closer)
        write_file(&notebooks, "pixi.toml", "[project]\nname = \"test\"");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(&notebooks, &all_kinds);
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::PixiToml);
        assert_eq!(found.path, notebooks.join("pixi.toml"));
    }

    #[test]
    fn test_closest_wins_env_yml_over_distant_pyproject() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        let sub = project.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // pyproject.toml far away
        write_file(&project, "pyproject.toml", "[project]\nname = \"test\"");
        // environment.yml closer
        write_file(&sub, "environment.yml", "name: test\ndependencies: []");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(&sub, &all_kinds);
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::EnvironmentYml);
    }

    #[test]
    fn test_tiebreaker_same_dir_pyproject_wins() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "pyproject.toml", "[project]\nname = \"test\"");
        write_file(temp.path(), "pixi.toml", "[project]\nname = \"test\"");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(temp.path(), &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::PyprojectToml);
    }

    #[test]
    fn test_tiebreaker_same_dir_pixi_over_env_yml() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "pixi.toml", "[project]\nname = \"test\"");
        write_file(temp.path(), "environment.yml", "name: test");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(temp.path(), &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::PixiToml);
    }

    #[test]
    fn test_no_project_files() {
        let temp = TempDir::new().unwrap();
        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(temp.path(), &all_kinds);
        assert!(found.is_none());
    }

    #[test]
    fn test_kinds_filter_skips_pyproject() {
        let temp = TempDir::new().unwrap();
        let sub = temp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // pyproject.toml right next to notebook
        write_file(&sub, "pyproject.toml", "[project]\nname = \"test\"");
        // pixi.toml one level up
        write_file(temp.path(), "pixi.toml", "[project]\nname = \"test\"");

        // When pyproject is excluded (uv not available), pixi should be found
        let no_pyproject = vec![ProjectFileKind::PixiToml, ProjectFileKind::EnvironmentYml];

        let found = find_nearest_project_file(&sub, &no_pyproject);
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::PixiToml);
        assert_eq!(found.path, temp.path().join("pixi.toml"));
    }

    #[test]
    fn test_stops_at_git_root() {
        let temp = TempDir::new().unwrap();
        let outer = temp.path().join("org");
        let repo = outer.join("my-repo");
        let notebooks = repo.join("notebooks");
        std::fs::create_dir_all(&notebooks).unwrap();

        // pyproject.toml above git root
        write_file(&outer, "pyproject.toml", "[project]\nname = \"org\"");
        // .git at repo root
        std::fs::create_dir(repo.join(".git")).unwrap();

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(&notebooks, &all_kinds);
        assert!(found.is_none());
    }

    #[test]
    fn test_finds_file_at_git_root() {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("my-repo");
        let notebooks = repo.join("notebooks");
        std::fs::create_dir_all(&notebooks).unwrap();

        write_file(&repo, "pixi.toml", "[project]\nname = \"test\"");
        std::fs::create_dir(repo.join(".git")).unwrap();

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(&notebooks, &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::PixiToml);
    }

    #[test]
    fn test_environment_yaml_variant() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "environment.yaml", "name: test");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(temp.path(), &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::EnvironmentYml);
    }

    #[test]
    fn test_yml_preferred_over_yaml() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "environment.yml", "name: yml");
        write_file(temp.path(), "environment.yaml", "name: yaml");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(temp.path(), &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().path, temp.path().join("environment.yml"));
    }

    #[test]
    fn test_from_file_path() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "pyproject.toml", "[project]\nname = \"test\"");
        let notebook = temp.path().join("notebook.ipynb");
        write_file(temp.path(), "notebook.ipynb", "{}");

        let all_kinds = vec![
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];

        let found = find_nearest_project_file(&notebook, &all_kinds);
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::PyprojectToml);
    }
}
