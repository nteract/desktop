//! Deno environment detection and configuration for notebook environments.
//!
//! This module handles:
//! - Detecting if Deno is installed (or bootstrapping via rattler if not)
//! - Finding deno.json/deno.jsonc configuration files
//! - Extracting Deno configuration from notebook metadata
//! - Managing Deno permissions for kernel execution

use crate::tools;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default value for flexible_npm_imports (true = auto-install npm packages)
fn default_flexible_npm_imports() -> bool {
    true
}

/// Deno permissions and configuration stored in notebook metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenoDependencies {
    /// Deno permission flags (e.g., ["--allow-net", "--allow-read"])
    #[serde(default)]
    pub permissions: Vec<String>,

    /// Path to import_map.json (relative to notebook or absolute)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_map: Option<String>,

    /// Path to deno.json config file (relative to notebook or absolute)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,

    /// When true (default), npm: imports auto-install packages.
    /// When false, uses packages from the project's node_modules.
    #[serde(default = "default_flexible_npm_imports")]
    pub flexible_npm_imports: bool,
}

impl Default for DenoDependencies {
    fn default() -> Self {
        Self {
            permissions: Vec::new(),
            import_map: None,
            config: None,
            flexible_npm_imports: true,
        }
    }
}

/// Configuration extracted from a deno.json file
#[derive(Debug, Clone)]
pub struct DenoConfig {
    /// Path to the deno.json file
    pub path: PathBuf,
    /// Project name from deno.json (if present)
    pub name: Option<String>,
    /// Import map URL/path
    pub import_map: Option<String>,
    /// Whether the config has imports defined
    pub has_imports: bool,
    /// Whether the config has tasks defined
    pub has_tasks: bool,
}

/// Serializable info about a detected deno.json for the frontend
#[derive(Debug, Clone, Serialize)]
pub struct DenoConfigInfo {
    /// Absolute path to the deno.json file
    pub path: String,
    /// Path relative to the notebook
    pub relative_path: String,
    /// Project name if available
    pub name: Option<String>,
    /// Whether the config has imports
    pub has_imports: bool,
    /// Whether the config has tasks
    pub has_tasks: bool,
}

// Raw deno.json structure for parsing
#[derive(Debug, Deserialize, Default)]
struct RawDenoConfig {
    name: Option<String>,
    #[serde(rename = "importMap")]
    import_map: Option<String>,
    imports: Option<serde_json::Value>,
    tasks: Option<serde_json::Value>,
}

/// Check if Deno is available on PATH or already cached.
///
/// This intentionally avoids triggering a full bootstrap during UI initialization,
/// because daemon kernel launch handles on-demand bootstrap when needed.
///
/// Returns true if:
/// - System deno exists and is version 2.x+, OR
/// - A cached deno binary exists (from GitHub download or rattler fallback)
pub async fn check_deno_available() -> bool {
    tools::check_deno_available_without_bootstrap().await
}

/// Check if Deno Jupyter support is available (Deno 1.37+)
///
/// Deno is auto-bootstrapped via rattler if not found on PATH.
pub async fn check_deno_jupyter_available() -> Result<bool> {
    let deno_path = tools::get_deno_path().await?;

    let output = tokio::process::Command::new(&deno_path)
        .args(["jupyter", "--help"])
        .output()
        .await
        .map_err(|e| anyhow!("Failed to check deno jupyter: {}", e))?;

    Ok(output.status.success())
}

/// Find a deno.json or deno.jsonc file by walking up from the given path
///
/// Starts from the given path (or its parent if it's a file) and walks up
/// the directory tree until a deno config is found or a stopping condition
/// is met (home directory or filesystem root).
pub fn find_deno_config(start_path: &Path) -> Option<PathBuf> {
    // Start from the directory containing the file, or the directory itself
    let start_dir = if start_path.is_file() {
        start_path.parent()?
    } else {
        start_path
    };

    let home_dir = dirs::home_dir();

    let mut current = start_dir.to_path_buf();
    loop {
        // Check for both deno.json and deno.jsonc
        for name in &["deno.json", "deno.jsonc"] {
            let candidate = current.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
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

/// Parse a deno.json file and extract relevant configuration
pub fn parse_deno_config(path: &Path) -> Result<DenoConfig> {
    let content =
        std::fs::read_to_string(path).map_err(|e| anyhow!("Failed to read deno.json: {}", e))?;

    // Handle JSONC (JSON with comments) by stripping comments
    let clean_content = strip_jsonc_comments(&content);

    let raw: RawDenoConfig = serde_json::from_str(&clean_content)
        .map_err(|e| anyhow!("Failed to parse deno.json: {}", e))?;

    Ok(DenoConfig {
        path: path.to_path_buf(),
        name: raw.name,
        import_map: raw.import_map,
        has_imports: raw.imports.is_some(),
        has_tasks: raw.tasks.is_some(),
    })
}

/// Create DenoConfigInfo from a config for sending to the frontend
pub fn create_deno_config_info(config: &DenoConfig, notebook_path: &Path) -> DenoConfigInfo {
    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    DenoConfigInfo {
        path: config.path.display().to_string(),
        relative_path,
        name: config.name.clone(),
        has_imports: config.has_imports,
        has_tasks: config.has_tasks,
    }
}

/// Extract Deno configuration from notebook metadata.
///
/// Checks `metadata.runt.deno` first (canonical location), then falls back to
/// legacy `metadata.deno` for backwards compatibility.
pub fn extract_deno_metadata(metadata: &nbformat::v4::Metadata) -> Option<DenoDependencies> {
    // Canonical location: metadata.runt.deno
    if let Some(runt) = metadata.additional.get("runt") {
        if let Some(deno) = runt.get("deno") {
            if let Ok(deps) = serde_json::from_value::<DenoDependencies>(deno.clone()) {
                return Some(deps);
            }
        }
    }

    // Legacy fallback: metadata.deno
    metadata
        .additional
        .get("deno")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Set Deno configuration in notebook metadata under `metadata.runt.deno`.
///
/// This is the canonical location for deno metadata, matching the pattern used
/// by UV (`runt.uv`) and Conda (`runt.conda`).
pub fn set_deno_metadata(metadata: &mut nbformat::v4::Metadata, deps: &DenoDependencies) {
    let deno_value = match serde_json::to_value(deps) {
        Ok(v) => v,
        Err(_) => return,
    };

    let runt = metadata
        .additional
        .entry("runt".to_string())
        .or_insert_with(|| serde_json::json!({"schema_version": "1"}));

    if let Some(runt_obj) = runt.as_object_mut() {
        runt_obj.insert("deno".to_string(), deno_value);
    }
}

/// Strip JSONC comments from content (single-line // and multi-line /* */)
fn strip_jsonc_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;

    while let Some(c) = chars.next() {
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }

        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }

        if !in_string && c == '/' {
            match chars.peek() {
                Some('/') => {
                    // Single-line comment - skip until newline
                    chars.next();
                    for cc in chars.by_ref() {
                        if cc == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    // Multi-line comment - skip until */
                    chars.next();
                    let mut prev_was_star = false;
                    for cc in chars.by_ref() {
                        if prev_was_star && cc == '/' {
                            break;
                        }
                        prev_was_star = cc == '*';
                    }
                }
                _ => {
                    result.push(c);
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_deno_config(dir: &Path, content: &str) {
        let path = dir.join("deno.json");
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_find_deno_config_same_dir() {
        let temp = TempDir::new().unwrap();
        create_deno_config(temp.path(), "{}");

        let found = find_deno_config(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("deno.json"));
    }

    #[test]
    fn test_find_deno_config_parent_dir() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("notebooks");
        std::fs::create_dir(&subdir).unwrap();
        create_deno_config(temp.path(), "{}");

        let found = find_deno_config(&subdir);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), temp.path().join("deno.json"));
    }

    #[test]
    fn test_find_deno_config_not_found() {
        let temp = TempDir::new().unwrap();
        let found = find_deno_config(temp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_find_deno_jsonc() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("deno.jsonc");
        std::fs::write(&path, "{}").unwrap();

        let found = find_deno_config(temp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), path);
    }

    #[test]
    fn test_parse_deno_config_minimal() {
        let temp = TempDir::new().unwrap();
        create_deno_config(temp.path(), "{}");

        let config = parse_deno_config(&temp.path().join("deno.json")).unwrap();
        assert!(config.name.is_none());
        assert!(!config.has_imports);
        assert!(!config.has_tasks);
    }

    #[test]
    fn test_parse_deno_config_with_fields() {
        let temp = TempDir::new().unwrap();
        create_deno_config(
            temp.path(),
            r#"{
                "name": "my-project",
                "importMap": "./import_map.json",
                "imports": {
                    "@std/": "https://deno.land/std@0.200.0/"
                },
                "tasks": {
                    "dev": "deno run --watch main.ts"
                }
            }"#,
        );

        let config = parse_deno_config(&temp.path().join("deno.json")).unwrap();
        assert_eq!(config.name, Some("my-project".to_string()));
        assert_eq!(config.import_map, Some("./import_map.json".to_string()));
        assert!(config.has_imports);
        assert!(config.has_tasks);
    }

    #[test]
    fn test_parse_deno_jsonc_with_comments() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("deno.jsonc");
        std::fs::write(
            &path,
            r#"{
                // This is a comment
                "name": "my-project",
                /* Multi-line
                   comment */
                "imports": {}
            }"#,
        )
        .unwrap();

        let config = parse_deno_config(&path).unwrap();
        assert_eq!(config.name, Some("my-project".to_string()));
        assert!(config.has_imports);
    }

    #[test]
    fn test_strip_jsonc_comments() {
        let input = r#"{
            // Single line comment
            "key": "value", // Trailing comment
            /* Block
               comment */
            "other": "test"
        }"#;

        let result = strip_jsonc_comments(input);
        assert!(!result.contains("//"));
        assert!(!result.contains("/*"));
        assert!(result.contains("\"key\""));
        assert!(result.contains("\"value\""));
    }

    #[test]
    fn test_strip_jsonc_preserves_urls() {
        let input = r#"{"url": "https://example.com"}"#;
        let result = strip_jsonc_comments(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_deno_dependencies_default() {
        let deps = DenoDependencies::default();
        assert!(deps.permissions.is_empty());
        assert!(deps.import_map.is_none());
        assert!(deps.config.is_none());
        assert!(deps.flexible_npm_imports); // Default is true
    }

    #[test]
    fn test_deno_dependencies_serde() {
        let deps = DenoDependencies {
            permissions: vec!["--allow-net".to_string(), "--allow-read".to_string()],
            import_map: Some("./import_map.json".to_string()),
            config: None,
            flexible_npm_imports: false,
        };

        let json = serde_json::to_string(&deps).unwrap();
        let parsed: DenoDependencies = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.permissions.len(), 2);
        assert_eq!(parsed.import_map, Some("./import_map.json".to_string()));
        assert!(!parsed.flexible_npm_imports);
    }

    #[test]
    fn test_create_deno_config_info() {
        let temp = TempDir::new().unwrap();
        let notebooks_dir = temp.path().join("notebooks");
        std::fs::create_dir(&notebooks_dir).unwrap();

        create_deno_config(temp.path(), r#"{"name": "my-project", "imports": {}}"#);

        let config = parse_deno_config(&temp.path().join("deno.json")).unwrap();
        let notebook_path = notebooks_dir.join("test.ipynb");
        let info = create_deno_config_info(&config, &notebook_path);

        assert_eq!(info.name, Some("my-project".to_string()));
        assert!(info.has_imports);
        let expected_path = std::path::Path::new("..").join("deno.json");
        assert_eq!(info.relative_path, expected_path.display().to_string());
    }
}
