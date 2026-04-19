//! environment.yml discovery and parsing for notebook environments.
//!
//! This module handles finding and parsing conda environment.yml files to extract
//! dependencies for notebook environments. Supports both conda dependencies
//! and pip dependencies from the `pip:` sub-list.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::conda_env::CondaDependencies;

/// Configuration extracted from an environment.yml file.
#[derive(Debug, Clone)]
pub struct EnvironmentYmlConfig {
    /// Path to the environment.yml file.
    pub path: PathBuf,
    /// Environment name from the `name:` field.
    pub name: Option<String>,
    /// Channels from the `channels:` field.
    pub channels: Vec<String>,
    /// Conda dependencies (excluding python).
    pub dependencies: Vec<String>,
    /// Pip dependencies from the nested `pip:` list.
    pub pip_dependencies: Vec<String>,
    /// Python version constraint extracted from the `python` dependency.
    pub python: Option<String>,
}

/// Serializable info about a detected environment.yml for the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvironmentYmlInfo {
    /// Absolute path to the environment.yml file.
    pub path: String,
    /// Path relative to the notebook.
    pub relative_path: String,
    /// Environment name if available.
    pub name: Option<String>,
    /// Whether there are conda dependencies.
    pub has_dependencies: bool,
    /// Number of conda dependencies.
    pub dependency_count: usize,
    /// Whether there are pip dependencies.
    pub has_pip_dependencies: bool,
    /// Number of pip dependencies.
    pub pip_dependency_count: usize,
    /// Python version constraint if specified.
    pub python: Option<String>,
    /// Conda channels.
    pub channels: Vec<String>,
}

// Raw YAML structure for parsing

#[derive(Debug, Deserialize)]
struct RawEnvironmentYml {
    name: Option<String>,
    channels: Option<Vec<String>>,
    dependencies: Option<Vec<serde_yaml::Value>>,
}

/// Find an environment.yml (or environment.yaml) file by walking up from the given path.
///
/// Starts from the given path (or its parent if it's a file) and walks up
/// the directory tree until an environment.yml is found or a stopping condition
/// is met (home directory or filesystem root).
///
/// Prefers `environment.yml` over `environment.yaml` when both exist in the
/// same directory.
pub fn find_environment_yml(start_path: &Path) -> Option<PathBuf> {
    // Start from the directory containing the file, or the directory itself
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();

    let mut current = start_dir.to_path_buf();
    loop {
        // Prefer .yml over .yaml
        let yml_candidate = current.join("environment.yml");
        if yml_candidate.exists() {
            return Some(yml_candidate);
        }

        let yaml_candidate = current.join("environment.yaml");
        if yaml_candidate.exists() {
            return Some(yaml_candidate);
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

/// Parse an environment.yml file and extract relevant configuration.
pub fn parse_environment_yml(path: &Path) -> Result<EnvironmentYmlConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read environment.yml: {}", e))?;

    let raw: RawEnvironmentYml = serde_yaml::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse environment.yml: {}", e))?;

    let name = raw.name;

    // Default channels to ["defaults"] if not specified
    let channels = raw.channels.unwrap_or_else(|| vec!["defaults".to_string()]);

    let mut dependencies = Vec::new();
    let mut pip_dependencies = Vec::new();
    let mut python = None;

    if let Some(deps) = raw.dependencies {
        for dep in deps {
            match dep {
                serde_yaml::Value::String(s) => {
                    // Check if this is the python dependency
                    if is_python_dep(&s) {
                        python = extract_python_version(&s);
                    } else {
                        dependencies.push(s);
                    }
                }
                serde_yaml::Value::Mapping(map) => {
                    // Check for pip: [...] mapping
                    if let Some(serde_yaml::Value::Sequence(pip_list)) =
                        map.get(serde_yaml::Value::String("pip".to_string()))
                    {
                        for pip_dep in pip_list {
                            if let serde_yaml::Value::String(s) = pip_dep {
                                pip_dependencies.push(s.clone());
                            }
                        }
                    }
                    // Other mapping keys (like prefix:) are ignored
                }
                _ => {
                    // Skip non-string, non-mapping entries
                }
            }
        }
    }

    Ok(EnvironmentYmlConfig {
        path: path.to_path_buf(),
        name,
        channels,
        dependencies,
        pip_dependencies,
        python,
    })
}

/// Check if a dependency string refers to the python package.
///
/// Matches "python", "python=3.10", "python>=3.9", etc.
fn is_python_dep(dep: &str) -> bool {
    let name = dep.split(['=', '>', '<', '!', ' ']).next().unwrap_or("");
    name == "python"
}

/// Extract the python version from a conda dependency string.
///
/// Handles formats like:
/// - "python=3.10" -> "3.10"
/// - "python>=3.9" -> "3.9"
/// - "python>=3.9,<4" -> "3.9"
/// - "python=3.10.*" -> "3.10"
fn extract_python_version(dep: &str) -> Option<String> {
    // Remove the "python" prefix and any operator
    let version_part = dep
        .trim_start_matches("python")
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches("==")
        .trim_start_matches("=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim();

    if version_part.is_empty() {
        return None;
    }

    // Take only the first version constraint (before any comma)
    let first_constraint = version_part.split(',').next().unwrap_or(version_part);

    // Remove trailing wildcard
    let cleaned = first_constraint.trim_end_matches(".*");

    // Extract major.minor
    let parts: Vec<&str> = cleaned.split('.').collect();
    if parts.len() >= 2 {
        Some(format!("{}.{}", parts[0], parts[1]))
    } else if !parts.is_empty() && !parts[0].is_empty() {
        Some(parts[0].to_string())
    } else {
        None
    }
}

/// Create EnvironmentYmlInfo from a config for sending to the frontend.
pub fn create_environment_yml_info(
    config: &EnvironmentYmlConfig,
    notebook_path: &Path,
) -> EnvironmentYmlInfo {
    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    EnvironmentYmlInfo {
        path: config.path.display().to_string(),
        relative_path,
        name: config.name.clone(),
        has_dependencies: !config.dependencies.is_empty(),
        dependency_count: config.dependencies.len(),
        has_pip_dependencies: !config.pip_dependencies.is_empty(),
        pip_dependency_count: config.pip_dependencies.len(),
        python: config.python.clone(),
        channels: config.channels.clone(),
    }
}

/// Convert an EnvironmentYmlConfig to CondaDependencies for use with rattler.
pub fn convert_to_conda_dependencies(config: &EnvironmentYmlConfig) -> CondaDependencies {
    CondaDependencies {
        dependencies: config.dependencies.clone(),
        channels: config.channels.clone(),
        python: config.python.clone(),
        env_id: None,
    }
}

/// Get all dependencies from an environment.yml config (conda + pip).
pub fn get_all_dependencies(config: &EnvironmentYmlConfig) -> (Vec<String>, Vec<String>) {
    (config.dependencies.clone(), config.pip_dependencies.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_environment_yml(dir: &Path, filename: &str, content: &str) {
        let path = dir.join(filename);
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    // ========================================================================
    // File discovery tests
    // ========================================================================

    #[test]
    fn test_find_environment_yml_same_dir() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "name: test\nchannels:\n  - defaults\n",
        );

        let found = find_environment_yml(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("environment.yml"));
    }

    #[test]
    fn test_find_environment_yml_parent_dir() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "name: test\nchannels:\n  - defaults\n",
        );

        let found = find_environment_yml(&subdir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("environment.yml"));
    }

    #[test]
    fn test_find_environment_yml_deeply_nested() {
        let temp = TempDir::new().unwrap();
        let deep_dir = temp.path().join("src").join("analysis").join("notebooks");
        std::fs::create_dir_all(&deep_dir).unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "name: test\nchannels:\n  - defaults\n",
        );

        let found = find_environment_yml(&deep_dir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("environment.yml"));
    }

    #[test]
    fn test_find_environment_yml_not_found() {
        let temp = TempDir::new().unwrap();
        let found = find_environment_yml(temp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_find_environment_yml_from_file_path() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "name: test\nchannels:\n  - defaults\n",
        );

        // Create a notebook file
        let notebook_path = subdir.join("analysis.ipynb");
        std::fs::write(&notebook_path, "{}").unwrap();

        // Should find environment.yml from a file path (not just directory)
        let found = find_environment_yml(&notebook_path);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("environment.yml"));
    }

    #[test]
    fn test_find_environment_yml_yaml_extension() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yaml",
            "name: test\nchannels:\n  - defaults\n",
        );

        let found = find_environment_yml(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("environment.yaml"));
    }

    #[test]
    fn test_find_environment_yml_prefers_yml_over_yaml() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "name: yml-version\nchannels:\n  - defaults\n",
        );
        create_environment_yml(
            temp.path(),
            "environment.yaml",
            "name: yaml-version\nchannels:\n  - defaults\n",
        );

        let found = find_environment_yml(temp.path());
        assert!(found.is_some());
        // Should find .yml, not .yaml
        assert_eq!(found.unwrap(), temp.path().join("environment.yml"));
    }

    #[test]
    fn test_find_environment_yml_stops_at_home_dir() {
        // This test verifies the home directory stop condition exists.
        // We can't easily create files above ~, but we can verify that
        // searching from a temp dir (which is under /tmp, not under ~)
        // doesn't find files that don't exist.
        let temp = TempDir::new().unwrap();
        let found = find_environment_yml(temp.path());
        assert!(found.is_none());
    }

    // ========================================================================
    // Parsing tests
    // ========================================================================

    #[test]
    fn test_parse_minimal() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.name, Some("myenv".to_string()));
        assert_eq!(config.channels, vec!["conda-forge"]);
        assert!(config.dependencies.is_empty());
        assert!(config.pip_dependencies.is_empty());
        assert!(config.python.is_none());
    }

    #[test]
    fn test_parse_with_conda_deps() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
dependencies:
  - numpy
  - pandas>=2.0
  - scipy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.dependencies.len(), 3);
        assert!(config.dependencies.contains(&"numpy".to_string()));
        assert!(config.dependencies.contains(&"pandas>=2.0".to_string()));
        assert!(config.dependencies.contains(&"scipy".to_string()));
    }

    #[test]
    fn test_parse_python_version_equals() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies:
  - python=3.10
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.python, Some("3.10".to_string()));
        // python should NOT be in the dependencies list
        assert!(!config.dependencies.iter().any(|d| d.contains("python")));
        assert_eq!(config.dependencies.len(), 1);
    }

    #[test]
    fn test_parse_python_version_gte() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies:
  - python>=3.9
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.python, Some("3.9".to_string()));
    }

    #[test]
    fn test_parse_python_version_complex() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies:
  - python>=3.9,<4
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.python, Some("3.9".to_string()));
    }

    #[test]
    fn test_parse_python_version_wildcard() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies:
  - python=3.10.*
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.python, Some("3.10".to_string()));
    }

    #[test]
    fn test_parse_python_not_in_deps() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies:
  - python=3.11
  - numpy
  - pandas
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.dependencies.len(), 2);
        assert!(!config.dependencies.iter().any(|d| d.contains("python")));
    }

    #[test]
    fn test_parse_with_pip_deps() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
dependencies:
  - numpy
  - pip:
    - requests>=2.0
    - fastapi
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.dependencies.len(), 1);
        assert!(config.dependencies.contains(&"numpy".to_string()));
        assert_eq!(config.pip_dependencies.len(), 2);
        assert!(config
            .pip_dependencies
            .contains(&"requests>=2.0".to_string()));
        assert!(config.pip_dependencies.contains(&"fastapi".to_string()));
    }

    #[test]
    fn test_parse_pip_mixed_with_conda() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
dependencies:
  - numpy
  - scipy
  - pip:
    - transformers
  - pandas
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        // Conda deps: numpy, scipy, pandas
        assert_eq!(config.dependencies.len(), 3);
        assert!(config.dependencies.contains(&"numpy".to_string()));
        assert!(config.dependencies.contains(&"scipy".to_string()));
        assert!(config.dependencies.contains(&"pandas".to_string()));
        // Pip deps: transformers
        assert_eq!(config.pip_dependencies.len(), 1);
        assert!(config
            .pip_dependencies
            .contains(&"transformers".to_string()));
    }

    #[test]
    fn test_parse_no_channels_defaults() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
dependencies:
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.channels, vec!["defaults"]);
    }

    #[test]
    fn test_parse_multiple_channels() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
  - defaults
  - bioconda
dependencies:
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.channels, vec!["conda-forge", "defaults", "bioconda"]);
    }

    #[test]
    fn test_parse_no_name() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
channels:
  - defaults
dependencies:
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert!(config.name.is_none());
    }

    #[test]
    fn test_parse_no_dependencies() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert!(config.dependencies.is_empty());
        assert!(config.pip_dependencies.is_empty());
        assert!(config.python.is_none());
    }

    #[test]
    fn test_parse_empty_dependencies() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
dependencies: []
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert!(config.dependencies.is_empty());
    }

    #[test]
    fn test_parse_ignores_prefix_key() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - defaults
prefix: /home/user/miniconda3/envs/myenv
dependencies:
  - numpy
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.dependencies.len(), 1);
        assert!(config.dependencies.contains(&"numpy".to_string()));
    }

    #[test]
    fn test_parse_version_specs() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            r#"
name: myenv
channels:
  - conda-forge
dependencies:
  - numpy>=1.20
  - pandas >=2.0,<3.0
  - scipy==1.11.0
  - matplotlib
"#,
        );

        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.dependencies.len(), 4);
        assert!(config.dependencies.contains(&"numpy>=1.20".to_string()));
        assert!(config
            .dependencies
            .contains(&"pandas >=2.0,<3.0".to_string()));
        assert!(config.dependencies.contains(&"scipy==1.11.0".to_string()));
        assert!(config.dependencies.contains(&"matplotlib".to_string()));
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let temp = TempDir::new().unwrap();
        create_environment_yml(
            temp.path(),
            "environment.yml",
            "{{{{invalid yaml content!!!!",
        );

        let result = parse_environment_yml(&temp.path().join("environment.yml"));
        assert!(result.is_err());
    }

    // ========================================================================
    // Conversion tests
    // ========================================================================

    #[test]
    fn test_convert_to_conda_dependencies() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: Some("myenv".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec!["numpy".to_string(), "pandas>=2.0".to_string()],
            pip_dependencies: vec!["requests".to_string()],
            python: Some("3.11".to_string()),
        };

        let conda_deps = convert_to_conda_dependencies(&config);
        assert_eq!(conda_deps.dependencies, config.dependencies);
        assert_eq!(conda_deps.channels, config.channels);
        assert_eq!(conda_deps.python, config.python);
        assert!(conda_deps.env_id.is_none());
    }

    #[test]
    fn test_convert_empty_deps() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: None,
            channels: vec!["defaults".to_string()],
            dependencies: vec![],
            pip_dependencies: vec![],
            python: None,
        };

        let conda_deps = convert_to_conda_dependencies(&config);
        assert!(conda_deps.dependencies.is_empty());
        assert_eq!(conda_deps.channels, vec!["defaults"]);
        assert!(conda_deps.python.is_none());
    }

    #[test]
    fn test_convert_preserves_channels() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: None,
            channels: vec![
                "conda-forge".to_string(),
                "defaults".to_string(),
                "bioconda".to_string(),
            ],
            dependencies: vec!["numpy".to_string()],
            pip_dependencies: vec![],
            python: None,
        };

        let conda_deps = convert_to_conda_dependencies(&config);
        assert_eq!(
            conda_deps.channels,
            vec!["conda-forge", "defaults", "bioconda"]
        );
    }

    // ========================================================================
    // Info creation tests
    // ========================================================================

    #[test]
    fn test_create_info_relative_path() {
        let temp = TempDir::new().unwrap();
        let notebooks_dir = temp.path().join("notebooks");
        std::fs::create_dir(&notebooks_dir).unwrap();

        let config = EnvironmentYmlConfig {
            path: temp.path().join("environment.yml"),
            name: Some("myenv".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec!["numpy".to_string(), "pandas".to_string()],
            pip_dependencies: vec!["requests".to_string()],
            python: Some("3.11".to_string()),
        };

        let notebook_path = notebooks_dir.join("test.ipynb");
        let info = create_environment_yml_info(&config, &notebook_path);

        let expected_path = std::path::Path::new("..").join("environment.yml");
        assert_eq!(info.relative_path, expected_path.display().to_string());
    }

    #[test]
    fn test_create_info_same_dir() {
        let temp = TempDir::new().unwrap();

        let config = EnvironmentYmlConfig {
            path: temp.path().join("environment.yml"),
            name: Some("myenv".to_string()),
            channels: vec!["defaults".to_string()],
            dependencies: vec!["numpy".to_string()],
            pip_dependencies: vec![],
            python: None,
        };

        let notebook_path = temp.path().join("test.ipynb");
        let info = create_environment_yml_info(&config, &notebook_path);

        assert_eq!(info.relative_path, "environment.yml");
    }

    #[test]
    fn test_create_info_counts() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: Some("myenv".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec![
                "numpy".to_string(),
                "pandas".to_string(),
                "scipy".to_string(),
            ],
            pip_dependencies: vec!["requests".to_string(), "fastapi".to_string()],
            python: Some("3.10".to_string()),
        };

        let notebook_path = PathBuf::from("/test/notebook.ipynb");
        let info = create_environment_yml_info(&config, &notebook_path);

        assert!(info.has_dependencies);
        assert_eq!(info.dependency_count, 3);
        assert!(info.has_pip_dependencies);
        assert_eq!(info.pip_dependency_count, 2);
        assert_eq!(info.python, Some("3.10".to_string()));
        assert_eq!(info.name, Some("myenv".to_string()));
    }

    #[test]
    fn test_create_info_no_deps() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: None,
            channels: vec!["defaults".to_string()],
            dependencies: vec![],
            pip_dependencies: vec![],
            python: None,
        };

        let notebook_path = PathBuf::from("/test/notebook.ipynb");
        let info = create_environment_yml_info(&config, &notebook_path);

        assert!(!info.has_dependencies);
        assert_eq!(info.dependency_count, 0);
        assert!(!info.has_pip_dependencies);
        assert_eq!(info.pip_dependency_count, 0);
    }

    // ========================================================================
    // Integration-style tests
    // ========================================================================

    #[test]
    fn test_get_all_dependencies() {
        let config = EnvironmentYmlConfig {
            path: PathBuf::from("/test/environment.yml"),
            name: Some("myenv".to_string()),
            channels: vec!["conda-forge".to_string()],
            dependencies: vec!["numpy".to_string(), "pandas".to_string()],
            pip_dependencies: vec!["requests".to_string()],
            python: Some("3.10".to_string()),
        };

        let (conda, pip) = get_all_dependencies(&config);
        assert_eq!(conda.len(), 2);
        assert_eq!(pip.len(), 1);
        assert!(conda.contains(&"numpy".to_string()));
        assert!(pip.contains(&"requests".to_string()));
    }

    // ========================================================================
    // Python version extraction unit tests
    // ========================================================================

    #[test]
    fn test_is_python_dep() {
        assert!(is_python_dep("python"));
        assert!(is_python_dep("python=3.10"));
        assert!(is_python_dep("python>=3.9"));
        assert!(is_python_dep("python>=3.9,<4"));
        assert!(is_python_dep("python=3.10.*"));
        assert!(!is_python_dep("numpy"));
        assert!(!is_python_dep("pythonnet"));
        assert!(!is_python_dep("cpython"));
    }

    #[test]
    fn test_extract_python_version_bare() {
        assert_eq!(extract_python_version("python"), None);
    }

    #[test]
    fn test_extract_python_version_equals() {
        assert_eq!(
            extract_python_version("python=3.10"),
            Some("3.10".to_string())
        );
    }

    #[test]
    fn test_extract_python_version_double_equals() {
        assert_eq!(
            extract_python_version("python==3.11"),
            Some("3.11".to_string())
        );
    }

    #[test]
    fn test_extract_python_version_gte() {
        assert_eq!(
            extract_python_version("python>=3.9"),
            Some("3.9".to_string())
        );
    }

    #[test]
    fn test_extract_python_version_complex_constraint() {
        assert_eq!(
            extract_python_version("python>=3.9,<4"),
            Some("3.9".to_string())
        );
    }

    #[test]
    fn test_extract_python_version_wildcard() {
        assert_eq!(
            extract_python_version("python=3.10.*"),
            Some("3.10".to_string())
        );
    }

    #[test]
    fn test_extract_python_version_major_only() {
        assert_eq!(extract_python_version("python=3"), Some("3".to_string()));
    }

    #[test]
    fn test_extract_python_version_full() {
        assert_eq!(
            extract_python_version("python=3.10.12"),
            Some("3.10".to_string())
        );
    }
}
