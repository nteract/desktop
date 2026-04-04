//! pixi.toml discovery and parsing for notebook environments.
//!
//! This module handles finding and parsing pixi.toml files to extract
//! dependencies for notebook environments. Supports both conda dependencies
//! from [dependencies] and PyPI dependencies from [pypi-dependencies].

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::conda_env::CondaDependencies;

/// Configuration extracted from a pixi.toml file.
#[derive(Debug, Clone)]
pub struct PixiConfig {
    /// Path to the pixi.toml file.
    pub path: PathBuf,
    /// Project/workspace name from [workspace.name] or [project.name].
    pub workspace_name: Option<String>,
    /// Channels from [workspace.channels] or [project.channels].
    pub channels: Vec<String>,
    /// Conda dependencies from [dependencies].
    pub dependencies: Vec<String>,
    /// PyPI dependencies from [pypi-dependencies].
    pub pypi_dependencies: Vec<String>,
    /// Python version constraint extracted from dependencies["python"].
    pub python: Option<String>,
}

/// Serializable info about a detected pixi.toml for the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PixiInfo {
    /// Absolute path to the pixi.toml file.
    pub path: String,
    /// Path relative to the notebook.
    pub relative_path: String,
    /// Project/workspace name if available.
    pub workspace_name: Option<String>,
    /// Whether [dependencies] has entries.
    pub has_dependencies: bool,
    /// Number of conda dependencies.
    pub dependency_count: usize,
    /// Whether [pypi-dependencies] has entries.
    pub has_pypi_dependencies: bool,
    /// Number of PyPI dependencies.
    pub pypi_dependency_count: usize,
    /// Python version constraint if specified.
    pub python: Option<String>,
    /// Conda channels.
    pub channels: Vec<String>,
}

// Raw TOML structures for parsing

#[derive(Debug, Deserialize, Default)]
struct RawPixiToml {
    workspace: Option<WorkspaceSection>,
    project: Option<ProjectSection>,
    dependencies: Option<HashMap<String, toml::Value>>,
    #[serde(rename = "pypi-dependencies")]
    pypi_dependencies: Option<HashMap<String, toml::Value>>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkspaceSection {
    name: Option<String>,
    channels: Option<Vec<String>>,
    #[allow(dead_code)]
    platforms: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct ProjectSection {
    name: Option<String>,
    channels: Option<Vec<String>>,
    #[allow(dead_code)]
    platforms: Option<Vec<String>>,
}

/// Find a pixi.toml file by walking up from the given path.
///
/// Starts from the given path (or its parent if it's a file) and walks up
/// the directory tree until a pixi.toml is found or a stopping condition
/// is met (home directory or filesystem root).
pub fn find_pixi_toml(start_path: &Path) -> Option<PathBuf> {
    // Start from the directory containing the file, or the directory itself
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();

    let mut current = start_dir.to_path_buf();
    loop {
        let candidate = current.join("pixi.toml");
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

/// Parse a pixi.toml file and extract relevant configuration.
pub fn parse_pixi_toml(path: &Path) -> Result<PixiConfig> {
    let content =
        std::fs::read_to_string(path).map_err(|e| anyhow!("Failed to read pixi.toml: {}", e))?;

    let raw: RawPixiToml =
        toml::from_str(&content).map_err(|e| anyhow!("Failed to parse pixi.toml: {}", e))?;

    // Get workspace/project name (prefer workspace over project for newer pixi)
    let workspace_name = raw
        .workspace
        .as_ref()
        .and_then(|w| w.name.clone())
        .or_else(|| raw.project.as_ref().and_then(|p| p.name.clone()));

    // Get channels (prefer workspace over project)
    let channels = raw
        .workspace
        .as_ref()
        .and_then(|w| w.channels.clone())
        .or_else(|| raw.project.as_ref().and_then(|p| p.channels.clone()))
        .unwrap_or_default();

    // Parse conda dependencies
    let mut dependencies = Vec::new();
    let mut python = None;

    if let Some(deps) = raw.dependencies {
        for (name, version) in deps {
            let spec = format_dependency_spec(&name, &version);

            // Extract python version separately
            if name == "python" {
                python = extract_python_version(&version);
            } else {
                dependencies.push(spec);
            }
        }
    }

    // Parse PyPI dependencies
    let mut pypi_dependencies = Vec::new();
    if let Some(deps) = raw.pypi_dependencies {
        for (name, version) in deps {
            let spec = format_pypi_dependency_spec(&name, &version);
            pypi_dependencies.push(spec);
        }
    }

    Ok(PixiConfig {
        path: path.to_path_buf(),
        workspace_name,
        channels,
        dependencies,
        pypi_dependencies,
        python,
    })
}

/// Format a conda dependency spec from name and version value.
fn format_dependency_spec(name: &str, version: &toml::Value) -> String {
    match version {
        toml::Value::String(v) => {
            if v == "*" || v.is_empty() {
                name.to_string()
            } else {
                // pixi uses conda-style specs: ">=1.0", "~=1.0", etc.
                format!("{}{}", name, v)
            }
        }
        toml::Value::Table(t) => {
            // Handle table format: { version = ">=1.0", channel = "..." }
            if let Some(toml::Value::String(v)) = t.get("version") {
                if v == "*" || v.is_empty() {
                    name.to_string()
                } else {
                    format!("{}{}", name, v)
                }
            } else {
                name.to_string()
            }
        }
        _ => name.to_string(),
    }
}

/// Format a PyPI dependency spec from name and version value.
fn format_pypi_dependency_spec(name: &str, version: &toml::Value) -> String {
    match version {
        toml::Value::String(v) => {
            if v == "*" || v.is_empty() {
                name.to_string()
            } else {
                format!("{}{}", name, v)
            }
        }
        toml::Value::Table(t) => {
            // Handle table format: { version = ">=1.0", extras = ["..."] }
            if let Some(toml::Value::String(v)) = t.get("version") {
                if v == "*" || v.is_empty() {
                    name.to_string()
                } else {
                    format!("{}{}", name, v)
                }
            } else {
                name.to_string()
            }
        }
        _ => name.to_string(),
    }
}

/// Extract python version from a dependency value.
fn extract_python_version(version: &toml::Value) -> Option<String> {
    match version {
        toml::Value::String(v) => {
            if v == "*" || v.is_empty() {
                None
            } else {
                // Remove leading operators for version constraint
                // e.g., ">=3.9" -> "3.9", ">3.9" -> "3.9"
                let trimmed = v
                    .trim_start_matches(">=")
                    .trim_start_matches("<=")
                    .trim_start_matches("==")
                    .trim_start_matches("~=")
                    .trim_start_matches('>')
                    .trim_start_matches('<')
                    .trim_start_matches('=');
                // Take just the major.minor version
                let parts: Vec<&str> = trimmed.split('.').collect();
                if parts.len() >= 2 {
                    Some(format!("{}.{}", parts[0], parts[1]))
                } else if !parts.is_empty() {
                    Some(parts[0].to_string())
                } else {
                    None
                }
            }
        }
        toml::Value::Table(t) => {
            if let Some(toml::Value::String(v)) = t.get("version") {
                extract_python_version(&toml::Value::String(v.clone()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Create PixiInfo from a config for sending to the frontend.
pub fn create_pixi_info(config: &PixiConfig, notebook_path: &Path) -> PixiInfo {
    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    PixiInfo {
        path: config.path.display().to_string(),
        relative_path,
        workspace_name: config.workspace_name.clone(),
        has_dependencies: !config.dependencies.is_empty(),
        dependency_count: config.dependencies.len(),
        has_pypi_dependencies: !config.pypi_dependencies.is_empty(),
        pypi_dependency_count: config.pypi_dependencies.len(),
        python: config.python.clone(),
        channels: config.channels.clone(),
    }
}

/// Convert a PixiConfig to CondaDependencies for use with rattler.
pub fn convert_to_conda_dependencies(config: &PixiConfig) -> CondaDependencies {
    CondaDependencies {
        dependencies: config.dependencies.clone(),
        channels: config.channels.clone(),
        python: config.python.clone(),
        env_id: None,
    }
}

/// Get all dependencies from a pixi config (conda + pypi).
pub fn get_all_dependencies(config: &PixiConfig) -> (Vec<String>, Vec<String>) {
    (
        config.dependencies.clone(),
        config.pypi_dependencies.clone(),
    )
}

impl PixiConfig {
    /// Check if this config has PyPI dependencies.
    pub fn has_pypi_dependencies(&self) -> bool {
        !self.pypi_dependencies.is_empty()
    }

    /// Check if ipykernel is declared in either conda or pypi dependencies.
    pub fn has_ipykernel(&self) -> bool {
        let check = |deps: &[String]| {
            deps.iter().any(|d| {
                let name = d
                    .split(['=', ' ', '<', '>', '!', ';', '['])
                    .next()
                    .unwrap_or(d);
                name.trim() == "ipykernel"
            })
        };
        check(&self.dependencies) || check(&self.pypi_dependencies)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_pixi_toml(dir: &Path, content: &str) {
        let path = dir.join("pixi.toml");
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_find_pixi_toml_same_dir() {
        let temp = TempDir::new().unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "test"
channels = ["conda-forge"]
platforms = ["linux-64"]
"#,
        );

        let found = find_pixi_toml(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pixi.toml"));
    }

    #[test]
    fn test_find_pixi_toml_parent_dir() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "test"
channels = ["conda-forge"]
platforms = ["linux-64"]
"#,
        );

        let found = find_pixi_toml(&subdir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pixi.toml"));
    }

    #[test]
    fn test_find_pixi_toml_not_found() {
        let temp = TempDir::new().unwrap();
        let found = find_pixi_toml(temp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_parse_pixi_toml_minimal() {
        let temp = TempDir::new().unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "myproject"
channels = ["conda-forge"]
platforms = ["linux-64"]
"#,
        );

        let config = parse_pixi_toml(&temp.path().join("pixi.toml")).unwrap();
        assert_eq!(config.workspace_name, Some("myproject".to_string()));
        assert_eq!(config.channels, vec!["conda-forge"]);
        assert!(config.dependencies.is_empty());
    }

    #[test]
    fn test_parse_pixi_toml_with_deps() {
        let temp = TempDir::new().unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "myproject"
channels = ["conda-forge"]
platforms = ["linux-64"]

[dependencies]
python = ">=3.10"
numpy = "*"
pandas = ">=2.0"
"#,
        );

        let config = parse_pixi_toml(&temp.path().join("pixi.toml")).unwrap();
        assert_eq!(config.workspace_name, Some("myproject".to_string()));
        assert_eq!(config.python, Some("3.10".to_string()));
        assert!(config.dependencies.iter().any(|d| d.contains("numpy")));
        assert!(config.dependencies.iter().any(|d| d.contains("pandas")));
        // Python should not be in dependencies list (extracted separately)
        assert!(!config.dependencies.iter().any(|d| d.contains("python")));
    }

    #[test]
    fn test_parse_pixi_toml_with_pypi_deps() {
        let temp = TempDir::new().unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "myproject"
channels = ["conda-forge"]
platforms = ["linux-64"]

[dependencies]
python = ">=3.10"

[pypi-dependencies]
requests = ">=2.0"
fastapi = "*"
"#,
        );

        let config = parse_pixi_toml(&temp.path().join("pixi.toml")).unwrap();
        assert!(config.has_pypi_dependencies());
        assert!(config
            .pypi_dependencies
            .iter()
            .any(|d| d.contains("requests")));
        assert!(config.pypi_dependencies.iter().any(|d| d == "fastapi"));
    }

    #[test]
    fn test_parse_pixi_toml_project_section() {
        // Older pixi format uses [project] instead of [workspace]
        let temp = TempDir::new().unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[project]
name = "oldproject"
channels = ["defaults"]
platforms = ["linux-64"]

[dependencies]
numpy = "*"
"#,
        );

        let config = parse_pixi_toml(&temp.path().join("pixi.toml")).unwrap();
        assert_eq!(config.workspace_name, Some("oldproject".to_string()));
        assert_eq!(config.channels, vec!["defaults"]);
    }

    #[test]
    fn test_convert_to_conda_dependencies() {
        let config = PixiConfig {
            path: PathBuf::from("/test/pixi.toml"),
            workspace_name: Some("test".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec!["numpy".to_string(), "pandas>=2.0".to_string()],
            pypi_dependencies: vec![],
            python: Some("3.11".to_string()),
        };

        let conda_deps = convert_to_conda_dependencies(&config);
        assert_eq!(conda_deps.dependencies, config.dependencies);
        assert_eq!(conda_deps.channels, config.channels);
        assert_eq!(conda_deps.python, config.python);
    }

    #[test]
    fn test_create_pixi_info() {
        let temp = TempDir::new().unwrap();
        let notebooks_dir = temp.path().join("notebooks");
        std::fs::create_dir(&notebooks_dir).unwrap();

        let config = PixiConfig {
            path: temp.path().join("pixi.toml"),
            workspace_name: Some("myproject".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec!["numpy".to_string(), "pandas".to_string()],
            pypi_dependencies: vec!["requests".to_string()],
            python: Some("3.11".to_string()),
        };

        let notebook_path = notebooks_dir.join("test.ipynb");
        let info = create_pixi_info(&config, &notebook_path);

        assert_eq!(info.workspace_name, Some("myproject".to_string()));
        assert!(info.has_dependencies);
        assert_eq!(info.dependency_count, 2);
        assert!(info.has_pypi_dependencies);
        assert_eq!(info.pypi_dependency_count, 1);
        assert_eq!(info.python, Some("3.11".to_string()));
        let expected_path = std::path::Path::new("..").join("pixi.toml");
        assert_eq!(info.relative_path, expected_path.display().to_string());
    }

    #[test]
    fn test_find_pixi_toml_from_file_path() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "test"
channels = ["conda-forge"]
platforms = ["linux-64"]
"#,
        );

        // Create a notebook file
        let notebook_path = subdir.join("analysis.ipynb");
        std::fs::write(&notebook_path, "{}").unwrap();

        // Should find pixi.toml from a file path (not just directory)
        let found = find_pixi_toml(&notebook_path);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pixi.toml"));
    }

    #[test]
    fn test_find_pixi_toml_deeply_nested() {
        let temp = TempDir::new().unwrap();
        let deep_dir = temp.path().join("src").join("analysis").join("notebooks");
        std::fs::create_dir_all(&deep_dir).unwrap();
        create_pixi_toml(
            temp.path(),
            r#"
[workspace]
name = "test"
channels = ["conda-forge"]
platforms = ["linux-64"]
"#,
        );

        let found = find_pixi_toml(&deep_dir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("pixi.toml"));
    }

    #[test]
    fn test_has_ipykernel_in_conda_deps() {
        let config = PixiConfig {
            path: PathBuf::from("pixi.toml"),
            workspace_name: None,
            channels: vec![],
            dependencies: vec!["python>=3.11".to_string(), "ipykernel".to_string()],
            pypi_dependencies: vec![],
            python: None,
        };
        assert!(config.has_ipykernel());
    }

    #[test]
    fn test_has_ipykernel_in_pypi_deps() {
        let config = PixiConfig {
            path: PathBuf::from("pixi.toml"),
            workspace_name: None,
            channels: vec![],
            dependencies: vec!["python>=3.11".to_string()],
            pypi_dependencies: vec!["ipykernel>=6.0".to_string()],
            python: None,
        };
        assert!(config.has_ipykernel());
    }

    #[test]
    fn test_has_ipykernel_missing() {
        let config = PixiConfig {
            path: PathBuf::from("pixi.toml"),
            workspace_name: None,
            channels: vec![],
            dependencies: vec!["python>=3.11".to_string(), "numpy".to_string()],
            pypi_dependencies: vec!["pandas".to_string()],
            python: None,
        };
        assert!(!config.has_ipykernel());
    }

    #[test]
    fn test_has_ipykernel_with_version_constraint() {
        let config = PixiConfig {
            path: PathBuf::from("pixi.toml"),
            workspace_name: None,
            channels: vec![],
            dependencies: vec!["ipykernel=6.29".to_string()],
            pypi_dependencies: vec![],
            python: None,
        };
        assert!(config.has_ipykernel());
    }
}
