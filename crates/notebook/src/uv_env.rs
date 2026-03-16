//! UV-based notebook metadata operations.
//!
//! This module provides notebook-specific metadata operations (set
//! dependencies in `nbformat::Metadata`) and a thin availability check.
//! Environment creation is handled entirely by the daemon via `kernel_env::uv`.

use serde::{Deserialize, Serialize};

/// Dependencies extracted from notebook metadata (uv format).
///
/// This is the notebook-side type that includes serde rename for
/// `requires-python`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookDependencies {
    pub dependencies: Vec<String>,
    #[serde(rename = "requires-python")]
    pub requires_python: Option<String>,
    /// UV prerelease strategy. When set, passes `--prerelease <value>` to uv pip install.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prerelease: Option<String>,
}

/// Set uv dependencies in notebook metadata (nested under runt).
pub fn set_dependencies(metadata: &mut nbformat::v4::Metadata, deps: &NotebookDependencies) {
    let uv_value = serde_json::json!({
        "dependencies": deps.dependencies,
        "requires-python": deps.requires_python,
    });

    let runt = metadata
        .additional
        .entry("runt".to_string())
        .or_insert_with(|| serde_json::json!({"schema_version": "1"}));

    if let Some(runt_obj) = runt.as_object_mut() {
        runt_obj.insert("uv".to_string(), uv_value);
    }
}

/// Check if uv is available (either on PATH or bootstrappable via rattler).
pub async fn check_uv_available() -> bool {
    kernel_env::uv::check_uv_available().await
}
