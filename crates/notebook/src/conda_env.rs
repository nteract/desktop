//! Conda-based notebook metadata operations.
//!
//! This module provides notebook-specific metadata operations (set
//! dependencies in `nbformat::Metadata`). Environment creation is
//! handled entirely by the daemon via `kernel_env::conda`.

use serde::{Deserialize, Serialize};

/// Dependencies extracted from notebook metadata (conda format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CondaDependencies {
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
}

/// Set conda dependencies in notebook metadata (nested under runt).
pub fn set_dependencies(metadata: &mut nbformat::v4::Metadata, deps: &CondaDependencies) {
    let conda_value = serde_json::json!({
        "dependencies": deps.dependencies,
        "channels": deps.channels,
        "python": deps.python,
    });

    let runt = metadata
        .additional
        .entry("runt".to_string())
        .or_insert_with(|| serde_json::json!({"schema_version": "1"}));

    if let Some(runt_obj) = runt.as_object_mut() {
        runt_obj.insert("conda".to_string(), conda_value);
    }
}
