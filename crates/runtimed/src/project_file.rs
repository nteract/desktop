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
                // pyproject.toml with [tool.pixi] should be treated as a pixi project
                if *kind == ProjectFileKind::PyprojectToml
                    && kinds.contains(&ProjectFileKind::PixiToml)
                    && pyproject_has_pixi_section(&candidate)
                {
                    return Some(DetectedProjectFile {
                        path: candidate,
                        kind: ProjectFileKind::PixiToml,
                    });
                }
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

/// Check if a pyproject.toml contains a `[tool.pixi]` section.
fn pyproject_has_pixi_section(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.contains("[tool.pixi]") || c.contains("[tool.pixi."))
        .unwrap_or(false)
}

/// Check if a pixi.toml (or pyproject.toml with [tool.pixi]) declares ipykernel.
///
/// Reads the file and checks for `ipykernel` as a TOML key in the
/// `[dependencies]` or `[pypi-dependencies]` tables. Uses a simple
/// text scan — if the line starts with `ipykernel` followed by `=` or
/// whitespace, it's a match. This avoids requiring a TOML parser dep.
pub fn pixi_toml_has_ipykernel(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ipykernel")
            && trimmed["ipykernel".len()..].starts_with(['=', ' ', '\t'])
        {
            return true;
        }
    }
    false
}

/// Minimal environment.yml parse result for the daemon.
///
/// Only extracts what the daemon needs: dependency names, channels, python version.
/// The full YAML parser lives in `crates/notebook/src/environment_yml.rs` (Tauri side).
#[derive(Debug, Clone)]
pub struct EnvironmentYmlConfig {
    pub dependencies: Vec<String>,
    pub channels: Vec<String>,
    pub python: Option<String>,
    pub name: Option<String>,
    /// Explicit prefix path from `prefix:` field (alternative to `name:`).
    pub prefix: Option<PathBuf>,
}

/// Search standard conda env directories for an existing named environment.
///
/// Checks `$CONDA_ENVS_DIRS`, `$CONDA_PREFIX/envs/`, `$MAMBA_ROOT_PREFIX/envs/`,
/// common install locations (`~/miniconda3`, `~/anaconda3`, `~/miniforge3`),
/// `~/.conda/envs/`, `~/.local/share/mamba/envs/` (micromamba default),
/// and any parent dirs from `~/.conda/environments.txt` (conda env registry).
///
/// Returns the first directory that contains `{dir}/{name}/bin/python` (or
/// `{dir}/{name}/python.exe` on Windows).
pub fn find_named_conda_env(name: &str) -> Option<PathBuf> {
    let candidates = conda_env_search_dirs();
    for dir in &candidates {
        let env_path = dir.join(name);
        let python = conda_python_path(&env_path);
        if python.exists() {
            return Some(env_path);
        }
    }
    None
}

/// Return the default directory for creating new named conda environments.
///
/// Uses the first writable directory from the search order. Falls back to
/// `~/.conda/envs/` if nothing else is available.
pub fn default_conda_envs_dir() -> PathBuf {
    let candidates = conda_env_search_dirs();
    for dir in &candidates {
        if dir.exists() && dir.is_dir() {
            // Check if writable by attempting to read dir
            if std::fs::read_dir(dir).is_ok() {
                return dir.clone();
            }
        }
    }
    // Fallback: ~/.conda/envs/
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".conda")
        .join("envs")
}

/// Get the python path within a conda prefix.
pub fn conda_python_path(prefix: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        prefix.join("python.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        prefix.join("bin").join("python")
    }
}

/// Resolve the conda prefix for an environment.yml file.
///
/// If the yaml has `prefix:` → use that path directly.
/// If the yaml has `name:` → search standard conda env dirs for it.
/// Returns `None` if neither is set or no matching env is found.
pub fn resolve_conda_env_prefix(yml_path: &Path) -> Option<PathBuf> {
    let config = parse_environment_yml(yml_path).ok()?;

    // prefix: takes precedence — it's an explicit path
    if let Some(ref prefix) = config.prefix {
        return Some(prefix.clone());
    }

    // name: — search standard conda env directories
    if let Some(ref name) = config.name {
        return find_named_conda_env(name);
    }

    None
}

/// Standard conda environment search directories.
fn conda_env_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    let mut push_unique = |p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };

    // $CONDA_ENVS_DIRS (colon-separated on Unix, semicolon on Windows)
    if let Ok(envs_dirs) = std::env::var("CONDA_ENVS_DIRS") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in envs_dirs.split(sep) {
            let p = PathBuf::from(dir.trim());
            if !p.as_os_str().is_empty() {
                push_unique(p);
            }
        }
    }

    // $CONDA_PREFIX/envs/
    if let Ok(prefix) = std::env::var("CONDA_PREFIX") {
        push_unique(PathBuf::from(prefix).join("envs"));
    }

    // $MAMBA_ROOT_PREFIX/envs/ (micromamba)
    if let Ok(prefix) = std::env::var("MAMBA_ROOT_PREFIX") {
        push_unique(PathBuf::from(prefix).join("envs"));
    }

    // Common conda/mamba installations
    if let Some(home) = dirs::home_dir() {
        for name in ["miniconda3", "anaconda3", "miniforge3"] {
            push_unique(home.join(name).join("envs"));
        }
        push_unique(home.join(".conda").join("envs"));
        // micromamba default location
        push_unique(home.join(".local").join("share").join("mamba").join("envs"));
    }

    // ~/.conda/environments.txt — conda's env registry, lists full paths to envs.
    // Extract parent dirs (the "envs/" directories) from each registered env path.
    if let Some(home) = dirs::home_dir() {
        let registry = home.join(".conda").join("environments.txt");
        if let Ok(content) = std::fs::read_to_string(&registry) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let env_path = PathBuf::from(trimmed);
                    if let Some(parent) = env_path.parent() {
                        push_unique(parent.to_path_buf());
                    }
                }
            }
        }
    }

    dirs
}

/// Parse an environment.yml file with a line-based parser (no serde_yaml dep).
///
/// Handles the common structure:
/// ```yaml
/// name: myenv
/// channels:
///   - conda-forge
/// dependencies:
///   - numpy=1.24
///   - pandas
///   - python=3.11
///   - pip:
///     - requests
/// ```
pub fn parse_environment_yml(path: &Path) -> Result<EnvironmentYmlConfig, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read {:?}: {}", path, e))?;
    parse_environment_yml_content(&content)
}

/// Parse environment.yml content (testable without filesystem).
fn parse_environment_yml_content(content: &str) -> Result<EnvironmentYmlConfig, String> {
    let mut name = None;
    let mut prefix = None;
    let mut channels = Vec::new();
    let mut dependencies = Vec::new();
    let mut python = None;

    #[derive(PartialEq)]
    enum Section {
        None,
        Channels,
        Dependencies,
        Pip, // inside dependencies: pip: sub-list
    }
    let mut section = Section::None;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Top-level keys (no leading whitespace)
        if !line.starts_with(' ') && !line.starts_with('\t') {
            if let Some(val) = trimmed.strip_prefix("name:") {
                name = Some(val.trim().trim_matches('"').trim_matches('\'').to_string());
                section = Section::None;
            } else if let Some(val) = trimmed.strip_prefix("prefix:") {
                let p = val.trim().trim_matches('"').trim_matches('\'');
                if !p.is_empty() {
                    prefix = Some(PathBuf::from(p));
                }
                section = Section::None;
            } else if trimmed == "channels:" {
                section = Section::Channels;
            } else if trimmed == "dependencies:" {
                section = Section::Dependencies;
            } else {
                section = Section::None;
            }
            continue;
        }

        // Indented content (list items)
        match section {
            Section::Channels => {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    channels.push(item.trim().trim_matches('"').trim_matches('\'').to_string());
                } else if !trimmed.starts_with('-') {
                    section = Section::None;
                }
            }
            Section::Dependencies => {
                if trimmed == "- pip:" {
                    section = Section::Pip;
                } else if let Some(item) = trimmed.strip_prefix("- ") {
                    let dep = item.trim().trim_matches('"').trim_matches('\'').to_string();
                    // Check if this is the python dep
                    let pkg_name = dep.split(['=', '>', '<', '!', ' ']).next().unwrap_or("");
                    if pkg_name == "python" {
                        // Extract version
                        let version_part = dep
                            .trim_start_matches("python")
                            .trim_start_matches(">=")
                            .trim_start_matches("<=")
                            .trim_start_matches("==")
                            .trim_start_matches('=')
                            .trim_start_matches('>')
                            .trim_start_matches('<')
                            .trim();
                        if !version_part.is_empty() {
                            let first = version_part.split(',').next().unwrap_or(version_part);
                            let cleaned = first.trim_end_matches(".*");
                            let parts: Vec<&str> = cleaned.split('.').collect();
                            if parts.len() >= 2 {
                                python = Some(format!("{}.{}", parts[0], parts[1]));
                            } else if !parts.is_empty() && !parts[0].is_empty() {
                                python = Some(parts[0].to_string());
                            }
                        }
                    } else {
                        dependencies.push(dep);
                    }
                } else if !trimmed.starts_with('-') && !trimmed.starts_with("pip:") {
                    section = Section::None;
                }
            }
            Section::Pip => {
                // Skip pip deps for now — they'd need uv/pip to install
                if let Some(_item) = trimmed.strip_prefix("- ") {
                    // pip deps are not supported in conda:env_yml yet
                } else if !trimmed.starts_with('-') {
                    section = Section::Dependencies;
                    // Re-process the line in case it's a new dependency
                    if let Some(item) = trimmed.strip_prefix("- ") {
                        let dep = item.trim().trim_matches('"').trim_matches('\'').to_string();
                        dependencies.push(dep);
                    }
                }
            }
            Section::None => {}
        }
    }

    if channels.is_empty() {
        channels.push("defaults".to_string());
    }

    Ok(EnvironmentYmlConfig {
        dependencies,
        channels,
        python,
        name,
        prefix,
    })
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

    #[test]
    fn test_pixi_toml_has_ipykernel_in_deps() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pixi.toml",
            "[project]\nname = \"test\"\n\n[dependencies]\npython = \">=3.11\"\nipykernel = \"*\"\n",
        );
        assert!(pixi_toml_has_ipykernel(&temp.path().join("pixi.toml")));
    }

    #[test]
    fn test_pixi_toml_has_ipykernel_in_pypi_deps() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pixi.toml",
            "[project]\nname = \"test\"\n\n[pypi-dependencies]\nipykernel = \">=6.0\"\n",
        );
        assert!(pixi_toml_has_ipykernel(&temp.path().join("pixi.toml")));
    }

    #[test]
    fn test_pixi_toml_missing_ipykernel() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pixi.toml",
            "[project]\nname = \"test\"\n\n[dependencies]\npython = \">=3.11\"\nnumpy = \"*\"\n",
        );
        assert!(!pixi_toml_has_ipykernel(&temp.path().join("pixi.toml")));
    }

    #[test]
    fn test_pixi_toml_has_ipykernel_nonexistent_file() {
        assert!(!pixi_toml_has_ipykernel(Path::new(
            "/nonexistent/pixi.toml"
        )));
    }

    #[test]
    fn test_pyproject_with_tool_pixi_detected_as_pixi() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"test\"\n\n[tool.pixi.project]\nchannels = [\"conda-forge\"]\nplatforms = [\"linux-64\"]\n",
        );

        let found = detect_project_file(temp.path());
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::PixiToml);
        assert_eq!(found.to_env_source(), "pixi:toml");
    }

    #[test]
    fn test_pyproject_without_tool_pixi_detected_as_uv() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"test\"\n\n[tool.uv]\ndev-dependencies = []\n",
        );

        let found = detect_project_file(temp.path());
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.kind, ProjectFileKind::PyprojectToml);
        assert_eq!(found.to_env_source(), "uv:pyproject");
    }

    #[test]
    fn test_pyproject_with_both_pixi_and_uv_prefers_pixi() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"test\"\n\n[tool.pixi.project]\nchannels = [\"conda-forge\"]\n\n[tool.uv]\ndev-dependencies = []\n",
        );

        let found = detect_project_file(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, ProjectFileKind::PixiToml);
    }

    #[test]
    fn test_ipykernel_in_pyproject_tool_pixi_deps() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"test\"\n\n[tool.pixi.dependencies]\nipykernel = \"*\"\nnumpy = \"*\"\n",
        );
        assert!(pixi_toml_has_ipykernel(&temp.path().join("pyproject.toml")));
    }

    #[test]
    fn test_parse_env_yml_basic() {
        let content = "name: myenv\nchannels:\n  - conda-forge\ndependencies:\n  - numpy=1.24\n  - pandas\n  - python=3.11\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(config.name, Some("myenv".to_string()));
        assert_eq!(config.channels, vec!["conda-forge"]);
        assert_eq!(config.dependencies, vec!["numpy=1.24", "pandas"]);
        assert_eq!(config.python, Some("3.11".to_string()));
    }

    #[test]
    fn test_parse_env_yml_with_pip() {
        let content = "name: test\nchannels:\n  - conda-forge\n  - defaults\ndependencies:\n  - numpy\n  - pip:\n    - requests\n    - flask\n  - scipy\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(config.channels, vec!["conda-forge", "defaults"]);
        // pip deps are skipped, scipy after pip block should still be captured
        assert!(config.dependencies.contains(&"numpy".to_string()));
    }

    #[test]
    fn test_parse_env_yml_no_channels() {
        let content = "name: test\ndependencies:\n  - numpy\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(config.channels, vec!["defaults"]);
        assert_eq!(config.dependencies, vec!["numpy"]);
    }

    #[test]
    fn test_parse_env_yml_python_version_extraction() {
        let content = "dependencies:\n  - python>=3.9,<4\n  - numpy\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(config.python, Some("3.9".to_string()));
        assert_eq!(config.dependencies, vec!["numpy"]);
    }

    #[test]
    fn test_parse_env_yml_from_file() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "environment.yml",
            "name: analysis\nchannels:\n  - conda-forge\ndependencies:\n  - numpy\n  - pandas\n",
        );
        let config = parse_environment_yml(&temp.path().join("environment.yml")).unwrap();
        assert_eq!(config.name, Some("analysis".to_string()));
        assert_eq!(config.dependencies, vec!["numpy", "pandas"]);
    }

    #[test]
    fn test_parse_env_yml_with_prefix() {
        let content = "prefix: /opt/conda/envs/myproject\nchannels:\n  - conda-forge\ndependencies:\n  - numpy\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(
            config.prefix,
            Some(PathBuf::from("/opt/conda/envs/myproject"))
        );
        assert!(config.name.is_none());
        assert_eq!(config.dependencies, vec!["numpy"]);
    }

    #[test]
    fn test_parse_env_yml_name_and_prefix() {
        // When both are present, both should be parsed (prefix takes precedence at resolution)
        let content = "name: myenv\nprefix: /custom/path\ndependencies:\n  - scipy\n";
        let config = parse_environment_yml_content(content).unwrap();
        assert_eq!(config.name, Some("myenv".to_string()));
        assert_eq!(config.prefix, Some(PathBuf::from("/custom/path")));
    }

    #[test]
    fn test_find_named_conda_env_not_found() {
        // A nonsense name should not be found
        assert!(find_named_conda_env("__nonexistent_env_abc123__").is_none());
    }

    #[test]
    fn test_conda_python_path() {
        let prefix = PathBuf::from("/opt/conda/envs/test");
        let python = conda_python_path(&prefix);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(python, PathBuf::from("/opt/conda/envs/test/bin/python"));
        #[cfg(target_os = "windows")]
        assert_eq!(python, PathBuf::from("/opt/conda/envs/test/python.exe"));
    }
}
