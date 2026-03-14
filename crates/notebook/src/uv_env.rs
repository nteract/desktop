//! UV-based environment management for notebook dependencies.
//!
//! This module provides notebook-specific metadata operations (extract, set,
//! remove dependencies from `nbformat::Metadata`) and delegates environment
//! creation to `kernel_env::uv`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Re-export core types from kernel-env for backward compatibility
pub use kernel_env::uv::UvEnvironment;

/// Dependencies extracted from notebook metadata (uv format).
///
/// This is the notebook-side type that includes serde rename for
/// `requires-python`. It converts to/from `kernel_env::UvDependencies`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookDependencies {
    pub dependencies: Vec<String>,
    #[serde(rename = "requires-python")]
    pub requires_python: Option<String>,
    /// UV prerelease strategy. When set, passes `--prerelease <value>` to uv pip install.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prerelease: Option<String>,
}

impl From<NotebookDependencies> for kernel_env::UvDependencies {
    fn from(deps: NotebookDependencies) -> Self {
        Self {
            dependencies: deps.dependencies,
            requires_python: deps.requires_python,
            prerelease: deps.prerelease,
        }
    }
}

impl From<kernel_env::UvDependencies> for NotebookDependencies {
    fn from(deps: kernel_env::UvDependencies) -> Self {
        Self {
            dependencies: deps.dependencies,
            requires_python: deps.requires_python,
            prerelease: deps.prerelease,
        }
    }
}

// =====================================================================
// Metadata operations (notebook-specific, depend on nbformat)
// =====================================================================

/// Extract dependencies from notebook metadata.
///
/// Looks for uv config in the new nested path `runt.uv` first,
/// then falls back to legacy `uv` for backward compatibility.
pub fn extract_dependencies(metadata: &nbformat::v4::Metadata) -> Option<NotebookDependencies> {
    // New format: metadata.runt.uv
    if let Some(runt_value) = metadata.additional.get("runt") {
        if let Some(uv_value) = runt_value.get("uv") {
            if let Ok(deps) = serde_json::from_value(uv_value.clone()) {
                return Some(deps);
            }
        }
    }
    // Legacy format: metadata.uv (fallback for unmigrated notebooks)
    let uv_value = metadata.additional.get("uv")?;
    serde_json::from_value(uv_value.clone()).ok()
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

/// Check if notebook has uv config (in new or legacy format).
pub fn has_uv_config(metadata: &nbformat::v4::Metadata) -> bool {
    if let Some(runt) = metadata.additional.get("runt") {
        if runt.get("uv").is_some() {
            return true;
        }
    }
    metadata.additional.contains_key("uv")
}

/// Remove uv config from metadata (both new and legacy paths).
pub fn remove_uv_config(metadata: &mut nbformat::v4::Metadata) {
    if let Some(runt) = metadata.additional.get_mut("runt") {
        if let Some(runt_obj) = runt.as_object_mut() {
            runt_obj.remove("uv");
        }
    }
    metadata.additional.remove("uv");
}

/// Extract the env_id from notebook metadata.
pub fn extract_env_id(metadata: &nbformat::v4::Metadata) -> Option<String> {
    let runt_value = metadata.additional.get("runt")?;
    runt_value.get("env_id")?.as_str().map(|s| s.to_string())
}

// =====================================================================
// Environment operations (delegating to kernel-env)
// =====================================================================

/// Check if uv is available (either on PATH or bootstrappable via rattler).
pub async fn check_uv_available() -> bool {
    kernel_env::uv::check_uv_available().await
}

/// Compute a cache key for the given dependencies.
pub fn compute_env_hash(deps: &NotebookDependencies, env_id: Option<&str>) -> String {
    kernel_env::uv::compute_env_hash(&deps.clone().into(), env_id)
}

/// Prepare a virtual environment with the given dependencies.
pub async fn prepare_environment(
    deps: &NotebookDependencies,
    env_id: Option<&str>,
) -> Result<UvEnvironment> {
    let handler: Arc<dyn kernel_env::ProgressHandler> = Arc::new(kernel_env::LogHandler);
    kernel_env::uv::prepare_environment(&deps.clone().into(), env_id, handler).await
}

/// Create a prewarmed environment with ipykernel, ipywidgets, and
/// user-configured default packages.
pub async fn create_prewarmed_environment() -> Result<UvEnvironment> {
    let extra: Vec<String> = crate::settings::load_settings().uv.default_packages;
    let handler: Arc<dyn kernel_env::ProgressHandler> = Arc::new(kernel_env::LogHandler);
    kernel_env::uv::create_prewarmed_environment(&extra, handler).await
}

/// Claim a prewarmed environment for a specific notebook.
pub async fn claim_prewarmed_environment(
    prewarmed: UvEnvironment,
    env_id: &str,
) -> Result<UvEnvironment> {
    kernel_env::uv::claim_prewarmed_environment(prewarmed, env_id).await
}

/// Find existing prewarmed environments from previous sessions.
pub async fn find_existing_prewarmed_environments() -> Vec<UvEnvironment> {
    kernel_env::uv::find_existing_prewarmed_environments().await
}

/// Warm up a UV environment by running Python to trigger .pyc compilation.
pub async fn warmup_uv_environment(env: &UvEnvironment) -> Result<()> {
    kernel_env::uv::warmup_environment(env).await
}

/// Check if a UV environment has been warmed up.
pub fn is_environment_warmed(env: &UvEnvironment) -> bool {
    kernel_env::uv::is_environment_warmed(env)
}

/// Copy an existing UV environment to a new location.
pub async fn copy_environment(source: &UvEnvironment, new_env_id: &str) -> Result<UvEnvironment> {
    kernel_env::uv::copy_environment(source, new_env_id).await
}

/// Install additional dependencies into an existing environment.
pub async fn sync_dependencies(env: &UvEnvironment, deps: &[String]) -> Result<()> {
    kernel_env::uv::sync_dependencies(env, deps).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_env_hash_stable() {
        let deps = NotebookDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: Some(">=3.10".to_string()),
            prerelease: None,
        };

        let hash1 = compute_env_hash(&deps, None);
        let hash2 = compute_env_hash(&deps, None);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_env_hash_order_independent() {
        let deps1 = NotebookDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let deps2 = NotebookDependencies {
            dependencies: vec!["numpy".to_string(), "pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };

        assert_eq!(
            compute_env_hash(&deps1, None),
            compute_env_hash(&deps2, None)
        );
    }

    #[test]
    fn test_compute_env_hash_different_deps() {
        let deps1 = NotebookDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let deps2 = NotebookDependencies {
            dependencies: vec!["numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };

        assert_ne!(
            compute_env_hash(&deps1, None),
            compute_env_hash(&deps2, None)
        );
    }
}
