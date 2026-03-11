//! pyproject.toml discovery and parsing for notebook environments.
//!
//! This module handles finding and parsing pyproject.toml files to extract
//! dependencies for notebook environments. Uses the `pyproject-toml` crate
//! from PyO3 for PEP 517/518/621 compliant parsing.

use anyhow::{anyhow, Result};
use pyproject_toml::PyProjectToml;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Configuration extracted from a pyproject.toml file.
#[derive(Debug, Clone)]
pub struct PyProjectConfig {
    /// Path to the pyproject.toml file.
    pub path: PathBuf,
    /// Project name from [project.name].
    pub project_name: Option<String>,
    /// Dependencies from [project.dependencies].
    pub dependencies: Vec<String>,
    /// Python version constraint from [project.requires-python].
    pub requires_python: Option<String>,
    /// Dev dependencies from [tool.uv.dev-dependencies].
    pub dev_dependencies: Vec<String>,
    /// Custom index URL from [tool.uv.index-url].
    pub index_url: Option<String>,
    /// Extra index URLs from [tool.uv.extra-index-url].
    pub extra_index_urls: Vec<String>,
}

/// Serializable info about a detected pyproject.toml for the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PyProjectInfo {
    /// Absolute path to the pyproject.toml file.
    pub path: String,
    /// Path relative to the notebook.
    pub relative_path: String,
    /// Project name if available.
    pub project_name: Option<String>,
    /// Whether [project.dependencies] has entries.
    pub has_dependencies: bool,
    /// Number of dependencies.
    pub dependency_count: usize,
    /// Whether [tool.uv.dev-dependencies] has entries.
    pub has_dev_dependencies: bool,
    /// Python version constraint if specified.
    pub requires_python: Option<String>,
    /// Whether a .venv directory exists in the project.
    pub has_venv: bool,
}

// [tool.uv] section - not covered by pyproject-toml crate
#[derive(Debug, Deserialize, Default)]
struct ToolUv {
    #[serde(rename = "dev-dependencies")]
    dev_dependencies: Option<Vec<String>>,
    #[serde(rename = "index-url")]
    index_url: Option<String>,
    #[serde(rename = "extra-index-url")]
    extra_index_url: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct ToolSection {
    uv: Option<ToolUv>,
}

#[derive(Debug, Deserialize, Default)]
struct RawPyProject {
    tool: Option<ToolSection>,
}

/// Find a pyproject.toml file by walking up from the given path.
///
/// Starts from the given path (or its parent if it's a file) and walks up
/// the directory tree until a pyproject.toml is found or a stopping condition
/// is met (home directory or filesystem root).
pub fn find_pyproject(start_path: &Path) -> Option<PathBuf> {
    // Start from the directory containing the file, or the directory itself
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();

    let mut current = start_dir.to_path_buf();
    loop {
        let candidate = current.join("pyproject.toml");
        if candidate.exists() {
            return Some(candidate);
        }

        // Stop at home directory or git repo root — a project file above the
        // repo root almost certainly belongs to a different project
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
            _ => return None, // Reached root
        }
    }
}

/// Parse a pyproject.toml file and extract relevant configuration.
///
/// Uses pyproject-toml crate for PEP 517/518/621 compliant parsing of
/// [project] section, and manual parsing for [tool.uv] section.
pub fn parse_pyproject(path: &Path) -> Result<PyProjectConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read pyproject.toml: {}", e))?;

    // Parse using pyproject-toml for PEP-compliant [project] section
    let parsed = PyProjectToml::new(&content)
        .map_err(|e| anyhow!("Failed to parse pyproject.toml: {}", e))?;

    // Extract [project] fields using the proper PEP 621 types
    let (project_name, dependencies, requires_python) = if let Some(project) = &parsed.project {
        let name = Some(project.name.clone());

        // Convert pep508 Requirements to strings
        let deps: Vec<String> = project
            .dependencies
            .as_ref()
            .map(|deps| deps.iter().map(|r| r.to_string()).collect())
            .unwrap_or_default();

        // Convert pep440 VersionSpecifiers to string
        let python = project.requires_python.as_ref().map(|v| v.to_string());

        (name, deps, python)
    } else {
        (None, vec![], None)
    };

    // Parse [tool.uv] section manually (not covered by pyproject-toml)
    let raw: RawPyProject = toml::from_str(&content).unwrap_or_default();
    let uv = raw.tool.and_then(|t| t.uv).unwrap_or_default();

    Ok(PyProjectConfig {
        path: path.to_path_buf(),
        project_name,
        dependencies,
        requires_python,
        dev_dependencies: uv.dev_dependencies.unwrap_or_default(),
        index_url: uv.index_url,
        extra_index_urls: uv.extra_index_url.unwrap_or_default(),
    })
}

/// Create PyProjectInfo from a config for sending to the frontend.
pub fn create_pyproject_info(config: &PyProjectConfig, notebook_path: &Path) -> PyProjectInfo {
    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    // Check if .venv exists in the project directory
    let has_venv = config
        .path
        .parent()
        .map(|dir| dir.join(".venv").is_dir())
        .unwrap_or(false);

    PyProjectInfo {
        path: config.path.display().to_string(),
        relative_path,
        project_name: config.project_name.clone(),
        has_dependencies: !config.dependencies.is_empty(),
        dependency_count: config.dependencies.len(),
        has_dev_dependencies: !config.dev_dependencies.is_empty(),
        requires_python: config.requires_python.clone(),
        has_venv,
    }
}

/// Get all dependencies from a pyproject config (main + dev).
pub fn get_all_dependencies(config: &PyProjectConfig) -> Vec<String> {
    let mut deps = config.dependencies.clone();
    deps.extend(config.dev_dependencies.clone());
    deps
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_pyproject(dir: &Path, content: &str) {
        let path = dir.join("pyproject.toml");
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_find_pyproject_same_dir() {
        let temp = TempDir::new().unwrap();
        create_pyproject(temp.path(), "[project]\nname = \"test\"");

        let found = find_pyproject(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pyproject.toml"));
    }

    #[test]
    fn test_find_pyproject_parent_dir() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_pyproject(temp.path(), "[project]\nname = \"test\"");

        let found = find_pyproject(&subdir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pyproject.toml"));
    }

    #[test]
    fn test_find_pyproject_not_found() {
        let temp = TempDir::new().unwrap();
        // No pyproject.toml created
        let found = find_pyproject(temp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_parse_pyproject_minimal() {
        let temp = TempDir::new().unwrap();
        create_pyproject(temp.path(), "[project]\nname = \"myproject\"");

        let config = parse_pyproject(&temp.path().join("pyproject.toml")).unwrap();
        assert_eq!(config.project_name, Some("myproject".to_string()));
        assert!(config.dependencies.is_empty());
    }

    #[test]
    fn test_parse_pyproject_with_deps() {
        let temp = TempDir::new().unwrap();
        create_pyproject(
            temp.path(),
            r#"
[project]
name = "myproject"
dependencies = ["pandas>=2.0", "numpy"]
requires-python = ">=3.10"

[tool.uv]
dev-dependencies = ["pytest", "ruff"]
index-url = "https://pypi.org/simple"
"#,
        );

        let config = parse_pyproject(&temp.path().join("pyproject.toml")).unwrap();
        assert_eq!(config.project_name, Some("myproject".to_string()));
        assert_eq!(config.dependencies.len(), 2);
        // PEP 508 normalized form may differ slightly
        assert!(config.dependencies.iter().any(|d| d.contains("pandas")));
        assert!(config.dependencies.iter().any(|d| d.contains("numpy")));
        assert_eq!(config.requires_python, Some(">=3.10".to_string()));
        assert_eq!(config.dev_dependencies, vec!["pytest", "ruff"]);
        assert_eq!(
            config.index_url,
            Some("https://pypi.org/simple".to_string())
        );
    }

    #[test]
    fn test_parse_pyproject_empty_sections() {
        let temp = TempDir::new().unwrap();
        // pyproject-toml requires at least [project] or [build-system]
        create_pyproject(temp.path(), "[build-system]\nrequires = []");

        let config = parse_pyproject(&temp.path().join("pyproject.toml")).unwrap();
        assert!(config.project_name.is_none());
        assert!(config.dependencies.is_empty());
    }

    #[test]
    fn test_find_pyproject_from_file_path() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_pyproject(temp.path(), "[project]\nname = \"test\"");

        // Create a notebook file
        let notebook_path = subdir.join("analysis.ipynb");
        std::fs::write(&notebook_path, "{}").unwrap();

        // Should find pyproject.toml from a file path (not just directory)
        let found = find_pyproject(&notebook_path);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pyproject.toml"));
    }

    #[test]
    fn test_find_pyproject_deeply_nested() {
        let temp = TempDir::new().unwrap();
        let deep_dir = temp.path().join("src").join("analysis").join("notebooks");
        std::fs::create_dir_all(&deep_dir).unwrap();
        create_pyproject(temp.path(), "[project]\nname = \"test\"");

        let found = find_pyproject(&deep_dir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pyproject.toml"));
    }

    #[test]
    fn test_find_pyproject_stops_at_git_root() {
        // Simulate: outer_dir has pyproject.toml, repo_dir has .git,
        // notebook is in repo_dir/notebooks/. The walk should stop at
        // repo_dir (git root) and NOT find the outer pyproject.toml.
        let temp = TempDir::new().unwrap();
        let outer = temp.path().join("org");
        let repo = outer.join("my-repo");
        let notebooks = repo.join("notebooks");
        std::fs::create_dir_all(&notebooks).unwrap();

        // Put pyproject.toml ABOVE the git root (in the org dir)
        create_pyproject(&outer, "[project]\nname = \"org-level\"");
        // Mark my-repo as a git root
        std::fs::create_dir(repo.join(".git")).unwrap();

        // Should NOT find the org-level pyproject.toml
        let found = find_pyproject(&notebooks);
        assert!(found.is_none());
    }

    #[test]
    fn test_find_pyproject_at_git_root() {
        // pyproject.toml at the same level as .git should still be found
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("my-repo");
        let notebooks = repo.join("notebooks");
        std::fs::create_dir_all(&notebooks).unwrap();

        create_pyproject(&repo, "[project]\nname = \"my-project\"");
        std::fs::create_dir(repo.join(".git")).unwrap();

        let found = find_pyproject(&notebooks);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), repo.join("pyproject.toml"));
    }

    #[test]
    fn test_create_pyproject_info() {
        let temp = TempDir::new().unwrap();
        let notebooks_dir = temp.path().join("notebooks");
        std::fs::create_dir(&notebooks_dir).unwrap();

        create_pyproject(
            temp.path(),
            r#"
[project]
name = "myproject"
dependencies = ["pandas", "numpy"]
requires-python = ">=3.10"

[tool.uv]
dev-dependencies = ["pytest"]
"#,
        );

        let config = parse_pyproject(&temp.path().join("pyproject.toml")).unwrap();
        let notebook_path = notebooks_dir.join("test.ipynb");
        let info = create_pyproject_info(&config, &notebook_path);

        assert_eq!(info.project_name, Some("myproject".to_string()));
        assert!(info.has_dependencies);
        assert_eq!(info.dependency_count, 2);
        assert!(info.has_dev_dependencies);
        assert_eq!(info.requires_python, Some(">=3.10".to_string()));
        let expected_path = std::path::Path::new("..").join("pyproject.toml");
        assert_eq!(info.relative_path, expected_path.display().to_string());
    }

    #[test]
    fn test_get_all_dependencies() {
        let temp = TempDir::new().unwrap();
        create_pyproject(
            temp.path(),
            r#"
[project]
name = "myproject"
dependencies = ["pandas", "numpy"]

[tool.uv]
dev-dependencies = ["pytest", "ruff"]
"#,
        );

        let config = parse_pyproject(&temp.path().join("pyproject.toml")).unwrap();
        let all_deps = get_all_dependencies(&config);

        assert_eq!(all_deps.len(), 4);
        assert!(all_deps.iter().any(|d| d.contains("pandas")));
        assert!(all_deps.iter().any(|d| d.contains("numpy")));
        assert!(all_deps.iter().any(|d| d == "pytest"));
        assert!(all_deps.iter().any(|d| d == "ruff"));
    }

    #[test]
    fn test_fixture_sample_project() {
        // Test against the actual fixture in fixtures/sample-project
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_dir = manifest_dir.join("fixtures").join("sample-project");
        let pyproject_path = fixture_dir.join("pyproject.toml");

        // Skip if fixture doesn't exist (e.g., in CI without fixtures)
        if !pyproject_path.exists() {
            return;
        }

        let config = parse_pyproject(&pyproject_path).unwrap();
        assert_eq!(config.project_name, Some("sample-project".to_string()));
        assert_eq!(config.requires_python, Some(">=3.10".to_string()));

        // Check dependencies
        assert!(config.dependencies.iter().any(|d| d.contains("httpx")));
        assert!(config.dependencies.iter().any(|d| d.contains("markupsafe")));
    }

    #[test]
    fn test_fixture_discovery_from_notebook() {
        // Test pyproject discovery from the notebook in fixtures/sample-project/notebooks/
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let notebook_path = manifest_dir
            .join("fixtures")
            .join("sample-project")
            .join("notebooks")
            .join("analysis.ipynb");

        // Skip if fixture doesn't exist
        if !notebook_path.exists() {
            return;
        }

        let found = find_pyproject(&notebook_path);
        assert!(found.is_some());

        let pyproject_path = found.unwrap();
        assert!(pyproject_path.ends_with("pyproject.toml"));
        assert!(pyproject_path.parent().unwrap().ends_with("sample-project"));
    }
}
