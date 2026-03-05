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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub fn merge_into_metadata_value(&self, metadata: &mut serde_json::Value) {
        let obj = match metadata.as_object_mut() {
            Some(o) => o,
            None => return,
        };

        // Replace kernelspec
        match &self.kernelspec {
            Some(ks) => {
                if let Ok(v) = serde_json::to_value(ks) {
                    obj.insert("kernelspec".to_string(), v);
                }
            }
            None => {
                obj.remove("kernelspec");
            }
        }

        // Merge language_info (preserve fields we don't track, like codemirror_mode)
        match &self.language_info {
            Some(li) => {
                if let Ok(v) = serde_json::to_value(li) {
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
            }
            None => {
                obj.remove("language_info");
            }
        }

        // Deep-merge runt namespace to preserve unknown fields (e.g. trust_signature)
        if let Ok(mut new_runt) = serde_json::to_value(&self.runt) {
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
        }
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
        }
    }
}

// ── Automerge document key ───────────────────────────────────────────

/// The key used to store the serialized `NotebookMetadataSnapshot` in the
/// Automerge document's `metadata` map.
pub const NOTEBOOK_METADATA_KEY: &str = "notebook_metadata";

#[cfg(test)]
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

        snapshot.merge_into_metadata_value(&mut metadata);

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
}
