//! Typed notebook metadata structs for Automerge sync.
//!
//! These types represent the notebook-level metadata that is synced between
//! the daemon and all connected notebook windows via the Automerge document.
//!
//! The `NotebookMetadataSnapshot` is serialized as a JSON string and stored
//! under the `metadata.notebook_metadata` key in the Automerge doc. When
//! writing to disk, it is merged back into the full `.ipynb` metadata,
//! preserving any fields we don't track (arbitrary Jupyter extensions, etc.).
//!
//! ## Merge semantics
//!
//! When saving to disk, the snapshot is merged into existing file metadata
//! like `Object.assign({}, existingMetadata, { kernelspec, language_info, runt })`.
//! This replaces `kernelspec`, `language_info`, and the `runt` key in
//! `metadata.additional` while leaving everything else untouched.

use serde::{Deserialize, Serialize};

// ── Runt namespace ───────────────────────────────────────────────────

/// Typed representation of the `metadata.runt` namespace in a notebook.
///
/// Contains environment configuration (uv, conda, deno), schema versioning,
/// a per-notebook environment ID, and trust signatures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntMetadata {
    /// Schema version for migration support. Currently "1".
    pub schema_version: String,

    /// Unique environment ID for this notebook (UUID).
    /// Used for per-notebook environment isolation when no dependencies are declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,

    /// UV (pip-compatible) inline dependency configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv: Option<UvInlineMetadata>,

    /// Conda inline dependency configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conda: Option<CondaInlineMetadata>,

    /// Deno runtime configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deno: Option<DenoMetadata>,

    /// HMAC-SHA256 signature of the dependency metadata for trust verification.
    /// Format: "hmac-sha256:<hex-digest>"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_signature: Option<String>,

    /// Timestamp when the notebook was last trusted (RFC 3339 format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_timestamp: Option<String>,

    /// Catch-all for unknown/third-party runt keys.
    /// Preserves fields we don't model (e.g. from newer schema versions or extensions)
    /// through deserialization → serialization round-trips.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// UV inline dependency metadata (`metadata.runt.uv`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UvInlineMetadata {
    /// PEP 508 dependency specifiers (e.g. `["pandas>=2.0", "numpy"]`).
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Python version constraint (e.g. `">=3.10"`).
    #[serde(
        rename = "requires-python",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub requires_python: Option<String>,
}

/// Conda inline dependency metadata (`metadata.runt.conda`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CondaInlineMetadata {
    /// Conda package names (e.g. `["numpy", "scipy"]`).
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Conda channels to search (e.g. `["conda-forge"]`).
    #[serde(default)]
    pub channels: Vec<String>,

    /// Explicit Python version for the conda environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

/// Deno runtime configuration (`metadata.runt.deno`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DenoMetadata {
    /// Deno permission flags (e.g. `["--allow-read", "--allow-write"]`).
    #[serde(default)]
    pub permissions: Vec<String>,

    /// Path to import_map.json (relative to notebook or absolute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_map: Option<String>,

    /// Path to deno.json config file (relative to notebook or absolute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,

    /// When true (default), npm: imports auto-install packages.
    /// When false, uses packages from the project's node_modules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flexible_npm_imports: Option<bool>,
}

// ── Notebook-level metadata snapshot ─────────────────────────────────

/// Snapshot of notebook-level metadata for Automerge sync.
///
/// Covers kernelspec + language_info + runt namespace — everything needed for
/// kernel detection and environment resolution. Serialized as JSON and stored
/// in the Automerge document under `metadata.notebook_metadata`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotebookMetadataSnapshot {
    /// Jupyter kernel specification (runtime type detection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernelspec: Option<KernelspecSnapshot>,

    /// Language information (set by the kernel after startup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_info: Option<LanguageInfoSnapshot>,

    /// Runt-specific metadata (dependencies, trust, environment config).
    pub runt: RuntMetadata,
}

/// Kernelspec snapshot for Automerge sync.
///
/// Mirrors the standard Jupyter `kernelspec` metadata fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecSnapshot {
    /// Kernel name (e.g. `"python3"`, `"deno"`).
    pub name: String,
    /// Human-readable display name (e.g. `"Python 3"`, `"Deno"`).
    pub display_name: String,
    /// Programming language (e.g. `"python"`, `"typescript"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Language info snapshot for Automerge sync.
///
/// Mirrors the standard Jupyter `language_info` metadata fields (subset).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LanguageInfoSnapshot {
    /// Language name (e.g. `"python"`, `"typescript"`).
    pub name: String,
    /// Language version (e.g. `"3.11.5"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ── Conversions to/from serde_json::Value ────────────────────────────

impl NotebookMetadataSnapshot {
    /// Build a snapshot from a raw `serde_json::Value` representing the full
    /// notebook-level metadata object (as read from an `.ipynb` file).
    ///
    /// Extracts `kernelspec`, `language_info`, and `runt` (with fallback to
    /// legacy `uv`/`conda` top-level keys).
    pub fn from_metadata_value(metadata: &serde_json::Value) -> Self {
        let kernelspec = metadata
            .get("kernelspec")
            .and_then(|v| serde_json::from_value::<KernelspecSnapshot>(v.clone()).ok());

        let language_info = metadata
            .get("language_info")
            .and_then(|v| serde_json::from_value::<LanguageInfoSnapshot>(v.clone()).ok());

        let runt = metadata
            .get("runt")
            .and_then(|v| serde_json::from_value::<RuntMetadata>(v.clone()).ok())
            .unwrap_or_else(|| {
                // Fallback: try legacy top-level uv/conda keys
                let uv = metadata
                    .get("uv")
                    .and_then(|v| serde_json::from_value::<UvInlineMetadata>(v.clone()).ok());
                let conda = metadata
                    .get("conda")
                    .and_then(|v| serde_json::from_value::<CondaInlineMetadata>(v.clone()).ok());

                RuntMetadata {
                    schema_version: "1".to_string(),
                    env_id: None,
                    uv,
                    conda,
                    deno: None,
                    trust_signature: None,
                    trust_timestamp: None,
                    extra: std::collections::HashMap::new(),
                }
            });

        NotebookMetadataSnapshot {
            kernelspec,
            language_info,
            runt,
        }
    }

    /// Merge this snapshot into a mutable JSON object representing the full
    /// notebook metadata. Replaces `kernelspec`, `language_info`, and `runt`
    /// while preserving all other keys.
    pub fn merge_into_metadata_value(
        &self,
        metadata: &mut serde_json::Value,
    ) -> Result<(), serde_json::Error> {
        let obj = match metadata.as_object_mut() {
            Some(o) => o,
            None => return Ok(()),
        };

        // Replace kernelspec
        match &self.kernelspec {
            Some(ks) => {
                let v = serde_json::to_value(ks)?;
                obj.insert("kernelspec".to_string(), v);
            }
            None => {
                obj.remove("kernelspec");
            }
        }

        // Merge language_info (preserve fields we don't track, like codemirror_mode)
        match &self.language_info {
            Some(li) => {
                let v = serde_json::to_value(li)?;
                if let Some(existing) = obj.get_mut("language_info") {
                    // Deep-merge: update tracked fields, keep the rest
                    if let Some(existing_obj) = existing.as_object_mut() {
                        if let Some(new_obj) = v.as_object() {
                            for (k, val) in new_obj {
                                existing_obj.insert(k.clone(), val.clone());
                            }
                        }
                    }
                } else {
                    obj.insert("language_info".to_string(), v);
                }
            }
            None => {
                obj.remove("language_info");
            }
        }

        // Deep-merge runt namespace to preserve unknown fields (e.g. trust_signature)
        let mut new_runt = serde_json::to_value(&self.runt)?;
        if let Some(existing_runt) = obj.get("runt") {
            // Merge: start with new snapshot, preserve existing keys not in snapshot
            if let (Some(existing_obj), Some(new_obj)) =
                (existing_runt.as_object(), new_runt.as_object_mut())
            {
                for (k, v) in existing_obj {
                    // Only keep existing keys that aren't in the new snapshot
                    if !new_obj.contains_key(k) {
                        new_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        obj.insert("runt".to_string(), new_runt);

        Ok(())
    }

    // ── Runtime detection ────────────────────────────────────────────

    /// Detect the notebook runtime from kernelspec + language_info metadata.
    ///
    /// Returns `"python"`, `"deno"`, or `None` for unknown runtimes.
    ///
    /// Priority chain:
    /// 1. `kernelspec.name` (substring match for "deno" or "python")
    /// 2. `kernelspec.language` (exact match: "typescript"/"javascript" → deno)
    /// 3. `language_info.name` (exact match, including "deno")
    /// 4. `runt.deno` presence (legacy notebooks without kernelspec)
    pub fn detect_runtime(&self) -> Option<String> {
        // Check kernelspec.name first (most reliable)
        if let Some(ref ks) = self.kernelspec {
            let name = ks.name.to_lowercase();
            if name.contains("deno") {
                return Some("deno".to_string());
            }
            if name.contains("python") {
                return Some("python".to_string());
            }
            // Check kernelspec.language
            if let Some(ref lang) = ks.language {
                let lang_lower = lang.to_lowercase();
                if lang_lower == "typescript" || lang_lower == "javascript" {
                    return Some("deno".to_string());
                }
                if lang_lower == "python" {
                    return Some("python".to_string());
                }
            }
        }

        // Fall back to language_info.name
        if let Some(ref li) = self.language_info {
            let name = li.name.to_lowercase();
            if name == "deno" || name == "typescript" || name == "javascript" {
                return Some("deno".to_string());
            }
            if name == "python" {
                return Some("python".to_string());
            }
        }

        // Fall back to runt.deno presence (legacy notebooks without kernelspec)
        if self.runt.deno.is_some() {
            return Some("deno".to_string());
        }

        None
    }

    // ── UV dependency operations ─────────────────────────────────────

    /// Add a UV dependency, deduplicating by package name (case-insensitive).
    /// Initializes the UV section if absent, preserving existing fields.
    pub fn add_uv_dependency(&mut self, pkg: &str) {
        let uv = self.runt.uv.get_or_insert_with(|| UvInlineMetadata {
            dependencies: Vec::new(),
            requires_python: None,
        });
        let name = extract_package_name(pkg);
        uv.dependencies.retain(|d| extract_package_name(d) != name);
        uv.dependencies.push(pkg.to_string());
    }

    /// Remove a UV dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_uv_dependency(&mut self, pkg: &str) -> bool {
        let Some(ref mut uv) = self.runt.uv else {
            return false;
        };
        let name = extract_package_name(pkg);
        let before = uv.dependencies.len();
        uv.dependencies.retain(|d| extract_package_name(d) != name);
        uv.dependencies.len() < before
    }

    /// Clear the UV section entirely (deps + requires-python).
    pub fn clear_uv_section(&mut self) {
        self.runt.uv = None;
    }

    /// Set UV requires-python constraint, preserving deps.
    /// Creates the UV section if it doesn't exist yet.
    pub fn set_uv_requires_python(&mut self, requires_python: Option<String>) {
        let uv = self.runt.uv.get_or_insert_with(|| UvInlineMetadata {
            dependencies: Vec::new(),
            requires_python: None,
        });
        uv.requires_python = requires_python;
    }

    /// Get UV dependencies, or empty slice if no UV section.
    pub fn uv_dependencies(&self) -> &[String] {
        self.runt
            .uv
            .as_ref()
            .map(|uv| uv.dependencies.as_slice())
            .unwrap_or(&[])
    }

    // ── Conda dependency operations ──────────────────────────────────

    /// Add a Conda dependency, deduplicating by package name (case-insensitive).
    /// Initializes the Conda section with `["conda-forge"]` channels if absent.
    pub fn add_conda_dependency(&mut self, pkg: &str) {
        let conda = self.runt.conda.get_or_insert_with(|| CondaInlineMetadata {
            dependencies: Vec::new(),
            channels: vec!["conda-forge".to_string()],
            python: None,
        });
        let name = extract_package_name(pkg);
        conda
            .dependencies
            .retain(|d| extract_package_name(d) != name);
        conda.dependencies.push(pkg.to_string());
    }

    /// Remove a Conda dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_conda_dependency(&mut self, pkg: &str) -> bool {
        let Some(ref mut conda) = self.runt.conda else {
            return false;
        };
        let name = extract_package_name(pkg);
        let before = conda.dependencies.len();
        conda
            .dependencies
            .retain(|d| extract_package_name(d) != name);
        conda.dependencies.len() < before
    }

    /// Clear the Conda section entirely.
    pub fn clear_conda_section(&mut self) {
        self.runt.conda = None;
    }

    /// Set Conda channels, preserving deps and python.
    /// Creates the Conda section if it doesn't exist yet.
    pub fn set_conda_channels(&mut self, channels: Vec<String>) {
        let conda = self.runt.conda.get_or_insert_with(|| CondaInlineMetadata {
            dependencies: Vec::new(),
            channels: Vec::new(),
            python: None,
        });
        conda.channels = channels;
    }

    /// Set Conda python version, preserving deps and channels.
    /// Creates the Conda section if it doesn't exist yet.
    pub fn set_conda_python(&mut self, python: Option<String>) {
        let conda = self.runt.conda.get_or_insert_with(|| CondaInlineMetadata {
            dependencies: Vec::new(),
            channels: vec!["conda-forge".to_string()],
            python: None,
        });
        conda.python = python;
    }

    /// Get Conda dependencies, or empty slice if no Conda section.
    pub fn conda_dependencies(&self) -> &[String] {
        self.runt
            .conda
            .as_ref()
            .map(|c| c.dependencies.as_slice())
            .unwrap_or(&[])
    }
}

impl RuntMetadata {
    /// Create a default RuntMetadata with UV configuration.
    pub fn new_uv(env_id: String) -> Self {
        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: Some(env_id),
            uv: Some(UvInlineMetadata {
                dependencies: Vec::new(),
                requires_python: None,
            }),
            conda: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::HashMap::new(),
        }
    }

    /// Create a default RuntMetadata with Conda configuration.
    pub fn new_conda(env_id: String) -> Self {
        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: Some(env_id),
            uv: None,
            conda: Some(CondaInlineMetadata {
                dependencies: Vec::new(),
                channels: vec!["conda-forge".to_string()],
                python: None,
            }),
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::HashMap::new(),
        }
    }

    /// Create a default RuntMetadata for Deno runtime.
    pub fn new_deno(env_id: String) -> Self {
        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: Some(env_id),
            uv: None,
            conda: None,
            deno: Some(DenoMetadata {
                permissions: Vec::new(),
                import_map: None,
                config: None,
                flexible_npm_imports: None,
            }),
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::HashMap::new(),
        }
    }
}

// ── Default implementations ──────────────────────────────────────────

impl Default for RuntMetadata {
    fn default() -> Self {
        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: None,
            conda: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::HashMap::new(),
        }
    }
}

// ── Package name extraction ──────────────────────────────────────────

/// Extract the base package name from a PEP 508 or conda dependency specifier.
///
/// Returns the lowercased package name, stripped of version constraints, extras,
/// environment markers, and whitespace.
///
/// # Examples
///
/// ```
/// use notebook_doc::metadata::extract_package_name;
/// assert_eq!(extract_package_name("pandas>=2.0"), "pandas");
/// assert_eq!(extract_package_name("requests[security]"), "requests");
/// assert_eq!(extract_package_name("NumPy"), "numpy");
/// ```
pub fn extract_package_name(spec: &str) -> String {
    spec.trim()
        .split(&['>', '<', '=', '!', '~', '[', ';', '@', ' '][..])
        .next()
        .unwrap_or(spec)
        .to_lowercase()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_runt_metadata_uv_roundtrip() {
        let meta = RuntMetadata::new_uv("test-env-id".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: RuntMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, parsed);
        assert_eq!(parsed.uv.as_ref().unwrap().dependencies.len(), 0);
    }

    #[test]
    fn test_runt_metadata_conda_roundtrip() {
        let meta = RuntMetadata::new_conda("test-env-id".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: RuntMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, parsed);
        assert_eq!(parsed.conda.as_ref().unwrap().channels, vec!["conda-forge"]);
    }

    #[test]
    fn test_runt_metadata_deno_roundtrip() {
        let meta = RuntMetadata::new_deno("test-env-id".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: RuntMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, parsed);
        assert!(parsed.deno.is_some());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: Some(LanguageInfoSnapshot {
                name: "python".to_string(),
                version: Some("3.11.5".to_string()),
            }),
            runt: RuntMetadata {
                schema_version: "1".to_string(),
                env_id: Some("abc-123".to_string()),
                uv: Some(UvInlineMetadata {
                    dependencies: vec!["pandas>=2.0".to_string(), "numpy".to_string()],
                    requires_python: Some(">=3.10".to_string()),
                }),
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::HashMap::new(),
            },
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: NotebookMetadataSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, parsed);
    }

    #[test]
    fn test_snapshot_from_metadata_value() {
        let metadata = serde_json::json!({
            "kernelspec": {
                "name": "python3",
                "display_name": "Python 3",
                "language": "python"
            },
            "runt": {
                "schema_version": "1",
                "env_id": "abc-123",
                "uv": {
                    "dependencies": ["pandas"],
                    "requires-python": ">=3.10"
                }
            },
            "jupyter": {
                "some_custom_field": true
            }
        });

        let snapshot = NotebookMetadataSnapshot::from_metadata_value(&metadata);
        assert_eq!(snapshot.kernelspec.as_ref().unwrap().name, "python3");
        assert_eq!(snapshot.runt.schema_version, "1");
        assert_eq!(
            snapshot.runt.uv.as_ref().unwrap().dependencies,
            vec!["pandas"]
        );
    }

    #[test]
    fn test_snapshot_from_legacy_metadata() {
        // Legacy format: uv at top level instead of inside runt
        let metadata = serde_json::json!({
            "kernelspec": {
                "name": "python3",
                "display_name": "Python 3"
            },
            "uv": {
                "dependencies": ["requests"],
                "requires-python": ">=3.9"
            }
        });

        let snapshot = NotebookMetadataSnapshot::from_metadata_value(&metadata);
        assert_eq!(
            snapshot.runt.uv.as_ref().unwrap().dependencies,
            vec!["requests"]
        );
        assert_eq!(snapshot.runt.schema_version, "1");
    }

    #[test]
    fn test_merge_into_preserves_unknown_keys() {
        let mut metadata = serde_json::json!({
            "kernelspec": {
                "name": "old_kernel",
                "display_name": "Old"
            },
            "jupyter": {
                "some_custom_field": true
            },
            "custom_extension": "preserved"
        });

        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: RuntMetadata::new_uv("env-1".to_string()),
        };

        snapshot.merge_into_metadata_value(&mut metadata).unwrap();

        // Kernelspec was replaced
        assert_eq!(metadata["kernelspec"]["name"], "python3");
        // language_info was removed (snapshot has None)
        assert!(metadata.get("language_info").is_none());
        // Unknown keys preserved
        assert_eq!(metadata["jupyter"]["some_custom_field"], true);
        assert_eq!(metadata["custom_extension"], "preserved");
        // Runt was added
        assert_eq!(metadata["runt"]["schema_version"], "1");
    }

    #[test]
    fn test_skip_serializing_none_fields() {
        let meta = RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: None,
            conda: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::HashMap::new(),
        };
        let json = serde_json::to_value(&meta).unwrap();
        // None fields should not appear in JSON
        assert!(!json.as_object().unwrap().contains_key("env_id"));
        assert!(!json.as_object().unwrap().contains_key("uv"));
        assert!(!json.as_object().unwrap().contains_key("conda"));
        assert!(!json.as_object().unwrap().contains_key("deno"));
        assert!(!json.as_object().unwrap().contains_key("trust_signature"));
        assert!(!json.as_object().unwrap().contains_key("trust_timestamp"));
        // schema_version should always be present
        assert!(json.as_object().unwrap().contains_key("schema_version"));
    }

    // ── extract_package_name ─────────────────────────────────────

    #[test]
    fn test_extract_package_name_simple() {
        assert_eq!(extract_package_name("pandas"), "pandas");
        assert_eq!(extract_package_name("numpy"), "numpy");
    }

    #[test]
    fn test_extract_package_name_version_specifiers() {
        assert_eq!(extract_package_name("pandas>=2.0"), "pandas");
        assert_eq!(extract_package_name("numpy==1.24.0"), "numpy");
        assert_eq!(extract_package_name("scipy<2"), "scipy");
        assert_eq!(extract_package_name("flask!=1.0"), "flask");
        assert_eq!(extract_package_name("django~=4.2"), "django");
    }

    #[test]
    fn test_extract_package_name_extras() {
        assert_eq!(extract_package_name("requests[security]"), "requests");
        assert_eq!(extract_package_name("pandas[sql,performance]"), "pandas");
    }

    #[test]
    fn test_extract_package_name_env_markers() {
        assert_eq!(
            extract_package_name("pywin32 ; sys_platform == 'win32'"),
            "pywin32"
        );
        assert_eq!(
            extract_package_name("numpy>=1.24;python_version>=\"3.8\""),
            "numpy"
        );
    }

    #[test]
    fn test_extract_package_name_at_url() {
        assert_eq!(
            extract_package_name("mypackage@https://example.com/pkg.tar.gz"),
            "mypackage"
        );
    }

    #[test]
    fn test_extract_package_name_case_insensitive() {
        assert_eq!(extract_package_name("NumPy"), "numpy");
        assert_eq!(extract_package_name("Pandas>=2.0"), "pandas");
        assert_eq!(extract_package_name("Flask"), "flask");
    }

    #[test]
    fn test_extract_package_name_empty() {
        assert_eq!(extract_package_name(""), "");
    }

    #[test]
    fn test_extract_package_name_whitespace() {
        assert_eq!(extract_package_name("  pandas  >=2.0"), "pandas");
    }

    // ── detect_runtime ───────────────────────────────────────────

    fn snapshot_with_kernelspec(name: &str, language: Option<&str>) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: Some(KernelspecSnapshot {
                name: name.to_string(),
                display_name: name.to_string(),
                language: language.map(String::from),
            }),
            language_info: None,
            runt: RuntMetadata::default(),
        }
    }

    fn snapshot_with_language_info(name: &str) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: Some(LanguageInfoSnapshot {
                name: name.to_string(),
                version: None,
            }),
            runt: RuntMetadata::default(),
        }
    }

    #[test]
    fn test_detect_runtime_kernelspec_python() {
        let s = snapshot_with_kernelspec("python3", Some("python"));
        assert_eq!(s.detect_runtime(), Some("python".to_string()));
    }

    #[test]
    fn test_detect_runtime_kernelspec_deno() {
        let s = snapshot_with_kernelspec("deno", None);
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_kernelspec_name_substring_match() {
        // "ir-python" contains "python"
        let s = snapshot_with_kernelspec("ir-python-kernel", None);
        assert_eq!(s.detect_runtime(), Some("python".to_string()));

        let s = snapshot_with_kernelspec("my-deno-kernel", None);
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_kernelspec_language_typescript() {
        let s = snapshot_with_kernelspec("custom-kernel", Some("typescript"));
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_kernelspec_language_javascript() {
        let s = snapshot_with_kernelspec("custom-kernel", Some("javascript"));
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_kernelspec_language_python() {
        let s = snapshot_with_kernelspec("custom-kernel", Some("python"));
        assert_eq!(s.detect_runtime(), Some("python".to_string()));
    }

    #[test]
    fn test_detect_runtime_language_info_python() {
        let s = snapshot_with_language_info("python");
        assert_eq!(s.detect_runtime(), Some("python".to_string()));
    }

    #[test]
    fn test_detect_runtime_language_info_deno() {
        let s = snapshot_with_language_info("deno");
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_language_info_typescript() {
        let s = snapshot_with_language_info("typescript");
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_language_info_javascript() {
        let s = snapshot_with_language_info("javascript");
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_runt_deno_fallback() {
        let mut s = NotebookMetadataSnapshot::default();
        s.runt.deno = Some(DenoMetadata {
            permissions: Vec::new(),
            import_map: None,
            config: None,
            flexible_npm_imports: None,
        });
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_none_for_empty_metadata() {
        let s = NotebookMetadataSnapshot::default();
        assert_eq!(s.detect_runtime(), None);
    }

    #[test]
    fn test_detect_runtime_kernelspec_takes_priority_over_language_info() {
        // kernelspec says python, language_info says typescript
        let s = NotebookMetadataSnapshot {
            kernelspec: Some(KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: None,
            }),
            language_info: Some(LanguageInfoSnapshot {
                name: "typescript".to_string(),
                version: None,
            }),
            runt: RuntMetadata::default(),
        };
        assert_eq!(s.detect_runtime(), Some("python".to_string()));
    }

    #[test]
    fn test_detect_runtime_case_insensitive() {
        let s = snapshot_with_kernelspec("Python3", Some("Python"));
        assert_eq!(s.detect_runtime(), Some("python".to_string()));

        let s = snapshot_with_kernelspec("DENO", None);
        assert_eq!(s.detect_runtime(), Some("deno".to_string()));
    }

    #[test]
    fn test_detect_runtime_unknown_kernelspec() {
        let s = snapshot_with_kernelspec("julia-1.10", Some("julia"));
        assert_eq!(s.detect_runtime(), None);
    }

    // ── UV dependency CRUD ───────────────────────────────────────

    #[test]
    fn test_add_uv_dependency_initializes_section() {
        let mut s = NotebookMetadataSnapshot::default();
        assert!(s.runt.uv.is_none());

        s.add_uv_dependency("pandas");
        assert_eq!(s.uv_dependencies(), &["pandas"]);
    }

    #[test]
    fn test_add_uv_dependency_deduplicates_by_name() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas>=1.0");
        s.add_uv_dependency("pandas>=2.0");
        assert_eq!(s.uv_dependencies(), &["pandas>=2.0"]);
    }

    #[test]
    fn test_add_uv_dependency_dedup_case_insensitive() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("NumPy");
        s.add_uv_dependency("numpy>=1.24");
        assert_eq!(s.uv_dependencies(), &["numpy>=1.24"]);
    }

    #[test]
    fn test_add_uv_dependency_preserves_requires_python() {
        let mut s = NotebookMetadataSnapshot::default();
        s.runt.uv = Some(UvInlineMetadata {
            dependencies: vec!["numpy".to_string()],
            requires_python: Some(">=3.10".to_string()),
        });

        s.add_uv_dependency("pandas");
        assert_eq!(
            s.runt.uv.as_ref().unwrap().requires_python,
            Some(">=3.10".to_string())
        );
        assert_eq!(s.uv_dependencies(), &["numpy", "pandas"]);
    }

    #[test]
    fn test_add_uv_dependency_multiple_packages() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas");
        s.add_uv_dependency("numpy");
        s.add_uv_dependency("scipy");
        assert_eq!(s.uv_dependencies(), &["pandas", "numpy", "scipy"]);
    }

    #[test]
    fn test_remove_uv_dependency_by_name() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas>=2.0");
        s.add_uv_dependency("numpy");

        assert!(s.remove_uv_dependency("pandas"));
        assert_eq!(s.uv_dependencies(), &["numpy"]);
    }

    #[test]
    fn test_remove_uv_dependency_case_insensitive() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas");
        assert!(s.remove_uv_dependency("Pandas"));
        assert!(s.uv_dependencies().is_empty());
    }

    #[test]
    fn test_remove_uv_dependency_version_agnostic() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas>=2.0");
        // Removing by bare name removes the versioned specifier
        assert!(s.remove_uv_dependency("pandas"));
        assert!(s.uv_dependencies().is_empty());
    }

    #[test]
    fn test_remove_uv_dependency_not_found() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas");
        assert!(!s.remove_uv_dependency("numpy"));
        assert_eq!(s.uv_dependencies(), &["pandas"]);
    }

    #[test]
    fn test_remove_uv_dependency_no_section() {
        let mut s = NotebookMetadataSnapshot::default();
        assert!(!s.remove_uv_dependency("pandas"));
    }

    #[test]
    fn test_clear_uv_section() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas");
        s.set_uv_requires_python(Some(">=3.10".to_string()));

        s.clear_uv_section();
        assert!(s.runt.uv.is_none());
        assert!(s.uv_dependencies().is_empty());
    }

    #[test]
    fn test_set_uv_requires_python() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_uv_dependency("pandas");

        s.set_uv_requires_python(Some(">=3.11".to_string()));
        assert_eq!(
            s.runt.uv.as_ref().unwrap().requires_python,
            Some(">=3.11".to_string())
        );

        s.set_uv_requires_python(None);
        assert_eq!(s.runt.uv.as_ref().unwrap().requires_python, None);
        // Deps still intact
        assert_eq!(s.uv_dependencies(), &["pandas"]);
    }

    #[test]
    fn test_set_uv_requires_python_creates_section() {
        let mut s = NotebookMetadataSnapshot::default();
        assert!(s.runt.uv.is_none());

        s.set_uv_requires_python(Some(">=3.10".to_string()));
        // UV section is created with empty deps
        assert!(s.runt.uv.is_some());
        assert_eq!(
            s.runt.uv.as_ref().unwrap().requires_python,
            Some(">=3.10".to_string())
        );
        assert!(s.uv_dependencies().is_empty());
    }

    #[test]
    fn test_uv_dependencies_empty_when_no_section() {
        let s = NotebookMetadataSnapshot::default();
        assert!(s.uv_dependencies().is_empty());
    }

    // ── Conda dependency CRUD ────────────────────────────────────

    #[test]
    fn test_add_conda_dependency_initializes_section() {
        let mut s = NotebookMetadataSnapshot::default();
        assert!(s.runt.conda.is_none());

        s.add_conda_dependency("numpy");
        assert_eq!(s.conda_dependencies(), &["numpy"]);
        assert_eq!(s.runt.conda.as_ref().unwrap().channels, vec!["conda-forge"]);
    }

    #[test]
    fn test_add_conda_dependency_deduplicates_by_name() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("scipy=1.10");
        s.add_conda_dependency("scipy=1.11");
        assert_eq!(s.conda_dependencies(), &["scipy=1.11"]);
    }

    #[test]
    fn test_add_conda_dependency_preserves_channels_and_python() {
        let mut s = NotebookMetadataSnapshot::default();
        s.runt.conda = Some(CondaInlineMetadata {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["conda-forge".to_string(), "bioconda".to_string()],
            python: Some("3.11".to_string()),
        });

        s.add_conda_dependency("scipy");
        let conda = s.runt.conda.as_ref().unwrap();
        assert_eq!(conda.channels, vec!["conda-forge", "bioconda"]);
        assert_eq!(conda.python, Some("3.11".to_string()));
        assert_eq!(s.conda_dependencies(), &["numpy", "scipy"]);
    }

    #[test]
    fn test_remove_conda_dependency_by_name() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("numpy");
        s.add_conda_dependency("scipy");

        assert!(s.remove_conda_dependency("numpy"));
        assert_eq!(s.conda_dependencies(), &["scipy"]);
    }

    #[test]
    fn test_remove_conda_dependency_not_found() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("numpy");
        assert!(!s.remove_conda_dependency("pandas"));
    }

    #[test]
    fn test_remove_conda_dependency_no_section() {
        let mut s = NotebookMetadataSnapshot::default();
        assert!(!s.remove_conda_dependency("numpy"));
    }

    #[test]
    fn test_clear_conda_section() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("numpy");
        s.set_conda_channels(vec!["bioconda".to_string()]);
        s.set_conda_python(Some("3.11".to_string()));

        s.clear_conda_section();
        assert!(s.runt.conda.is_none());
        assert!(s.conda_dependencies().is_empty());
    }

    #[test]
    fn test_set_conda_channels() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("numpy");

        s.set_conda_channels(vec!["conda-forge".to_string(), "bioconda".to_string()]);
        assert_eq!(
            s.runt.conda.as_ref().unwrap().channels,
            vec!["conda-forge", "bioconda"]
        );
        // Deps still intact
        assert_eq!(s.conda_dependencies(), &["numpy"]);
    }

    #[test]
    fn test_set_conda_channels_creates_section() {
        let mut s = NotebookMetadataSnapshot::default();
        s.set_conda_channels(vec!["bioconda".to_string()]);
        assert!(s.runt.conda.is_some());
        assert_eq!(s.runt.conda.as_ref().unwrap().channels, vec!["bioconda"]);
    }

    #[test]
    fn test_set_conda_python() {
        let mut s = NotebookMetadataSnapshot::default();
        s.add_conda_dependency("numpy");

        s.set_conda_python(Some("3.12".to_string()));
        assert_eq!(
            s.runt.conda.as_ref().unwrap().python,
            Some("3.12".to_string())
        );

        s.set_conda_python(None);
        assert_eq!(s.runt.conda.as_ref().unwrap().python, None);
        // Deps still intact
        assert_eq!(s.conda_dependencies(), &["numpy"]);
    }

    #[test]
    fn test_conda_dependencies_empty_when_no_section() {
        let s = NotebookMetadataSnapshot::default();
        assert!(s.conda_dependencies().is_empty());
    }

    // ── Default impls ────────────────────────────────────────────

    #[test]
    fn test_runt_metadata_default() {
        let meta = RuntMetadata::default();
        assert_eq!(meta.schema_version, "1");
        assert!(meta.uv.is_none());
        assert!(meta.conda.is_none());
        assert!(meta.deno.is_none());
        assert!(meta.env_id.is_none());
    }

    #[test]
    fn test_notebook_metadata_snapshot_default() {
        let s = NotebookMetadataSnapshot::default();
        assert!(s.kernelspec.is_none());
        assert!(s.language_info.is_none());
        assert_eq!(s.runt.schema_version, "1");
    }
}
