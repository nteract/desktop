//! Conda-based environment management for notebook dependencies.
//!
//! This module provides notebook-specific metadata operations (extract, set,
//! remove dependencies from `nbformat::Metadata`) and delegates environment
//! creation to `kernel_env::conda`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

// Re-export core types from kernel-env for backward compatibility
pub use kernel_env::conda::CondaEnvironment;
pub use kernel_env::progress::EnvProgressPhase;

/// Dependencies extracted from notebook metadata (conda format).
///
/// This is the notebook-side type that includes env_id for per-notebook
/// isolation. It converts to/from `kernel_env::CondaDependencies`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CondaDependencies {
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
}

impl From<CondaDependencies> for kernel_env::CondaDependencies {
    fn from(deps: CondaDependencies) -> Self {
        Self {
            dependencies: deps.dependencies,
            channels: deps.channels,
            python: deps.python,
            env_id: deps.env_id,
        }
    }
}

impl From<kernel_env::CondaDependencies> for CondaDependencies {
    fn from(deps: kernel_env::CondaDependencies) -> Self {
        Self {
            dependencies: deps.dependencies,
            channels: deps.channels,
            python: deps.python,
            env_id: deps.env_id,
        }
    }
}

/// Full progress event payload sent to frontend.
#[derive(Debug, Clone, Serialize)]
pub struct EnvProgressEvent {
    pub env_type: String,
    #[serde(flatten)]
    pub phase: EnvProgressPhase,
}

/// Progress handler that emits Tauri events.
pub struct TauriProgressHandler {
    app: AppHandle,
}

impl TauriProgressHandler {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl kernel_env::ProgressHandler for TauriProgressHandler {
    fn on_progress(&self, env_type: &str, phase: EnvProgressPhase) {
        let event = EnvProgressEvent {
            env_type: env_type.to_string(),
            phase,
        };
        if let Err(e) = self.app.emit("env:progress", &event) {
            log::warn!("Failed to emit env:progress event: {}", e);
        }
    }
}

// =====================================================================
// Metadata operations (notebook-specific, depend on nbformat)
// =====================================================================

/// Extract dependencies from notebook metadata.
pub fn extract_dependencies(metadata: &nbformat::v4::Metadata) -> Option<CondaDependencies> {
    // New format: metadata.runt.conda
    if let Some(runt_value) = metadata.additional.get("runt") {
        if let Some(conda_value) = runt_value.get("conda") {
            if let Ok(deps) = serde_json::from_value(conda_value.clone()) {
                return Some(deps);
            }
        }
    }
    // Legacy format: metadata.conda
    let conda_value = metadata.additional.get("conda")?;
    serde_json::from_value(conda_value.clone()).ok()
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

/// Check if notebook has conda config (in new or legacy format).
pub fn has_conda_config(metadata: &nbformat::v4::Metadata) -> bool {
    if let Some(runt) = metadata.additional.get("runt") {
        if runt.get("conda").is_some() {
            return true;
        }
    }
    metadata.additional.contains_key("conda")
}

/// Remove conda config from metadata (both new and legacy paths).
pub fn remove_conda_config(metadata: &mut nbformat::v4::Metadata) {
    if let Some(runt) = metadata.additional.get_mut("runt") {
        if let Some(runt_obj) = runt.as_object_mut() {
            runt_obj.remove("conda");
        }
    }
    metadata.additional.remove("conda");
}

// =====================================================================
// Environment operations (delegating to kernel-env)
// =====================================================================

/// Compute a cache key for the given dependencies.
pub fn compute_env_hash(deps: &CondaDependencies) -> String {
    kernel_env::conda::compute_env_hash(&deps.clone().into())
}

/// Prepare a conda environment with the given dependencies.
pub async fn prepare_environment(
    deps: &CondaDependencies,
    app: Option<&AppHandle>,
) -> Result<CondaEnvironment> {
    let handler: Arc<dyn kernel_env::ProgressHandler> = match app {
        Some(a) => Arc::new(TauriProgressHandler::new(a.clone())),
        None => Arc::new(kernel_env::LogHandler),
    };
    kernel_env::conda::prepare_environment(&deps.clone().into(), handler).await
}

/// Create a prewarmed conda environment.
pub async fn create_prewarmed_conda_environment(
    app: Option<&AppHandle>,
) -> Result<CondaEnvironment> {
    let extra: Vec<String> = crate::settings::load_settings().conda.default_packages;
    let handler: Arc<dyn kernel_env::ProgressHandler> = match app {
        Some(a) => Arc::new(TauriProgressHandler::new(a.clone())),
        None => Arc::new(kernel_env::LogHandler),
    };
    kernel_env::conda::create_prewarmed_environment(&extra, handler).await
}

/// Warm up a conda environment by running Python to trigger .pyc compilation.
pub async fn warmup_conda_environment(env: &CondaEnvironment) -> Result<()> {
    kernel_env::conda::warmup_environment(env).await
}

/// Check if a conda environment has been warmed up.
pub fn is_environment_warmed(env: &CondaEnvironment) -> bool {
    kernel_env::conda::is_environment_warmed(env)
}

/// Claim a prewarmed conda environment for a specific notebook.
pub async fn claim_prewarmed_conda_environment(
    prewarmed: CondaEnvironment,
    env_id: &str,
) -> Result<CondaEnvironment> {
    kernel_env::conda::claim_prewarmed_environment(prewarmed, env_id).await
}

/// Find existing prewarmed conda environments from previous sessions.
pub async fn find_existing_prewarmed_conda_environments() -> Vec<CondaEnvironment> {
    kernel_env::conda::find_existing_prewarmed_environments().await
}

/// Install additional dependencies into an existing environment.
pub async fn sync_dependencies(env: &CondaEnvironment, deps: &CondaDependencies) -> Result<()> {
    kernel_env::conda::sync_dependencies(env, &deps.clone().into()).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_env_hash_stable() {
        let deps = CondaDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: Some("3.11".to_string()),
            env_id: Some("test-env-id".to_string()),
        };

        let hash1 = compute_env_hash(&deps);
        let hash2 = compute_env_hash(&deps);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_env_hash_order_independent() {
        let deps1 = CondaDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            channels: vec![],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        let deps2 = CondaDependencies {
            dependencies: vec!["numpy".to_string(), "pandas".to_string()],
            channels: vec![],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        assert_eq!(compute_env_hash(&deps1), compute_env_hash(&deps2));
    }

    #[test]
    fn test_compute_env_hash_different_deps() {
        let deps1 = CondaDependencies {
            dependencies: vec!["pandas".to_string()],
            channels: vec![],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        let deps2 = CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec![],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        assert_ne!(compute_env_hash(&deps1), compute_env_hash(&deps2));
    }

    #[test]
    fn test_compute_env_hash_includes_channels() {
        let deps1 = CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        let deps2 = CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["defaults".to_string()],
            python: None,
            env_id: Some("test-env-1".to_string()),
        };

        assert_ne!(compute_env_hash(&deps1), compute_env_hash(&deps2));
    }

    #[test]
    fn test_compute_env_hash_different_env_id() {
        let deps1 = CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: Some("notebook-1".to_string()),
        };

        let deps2 = CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: Some("notebook-2".to_string()),
        };

        assert_ne!(compute_env_hash(&deps1), compute_env_hash(&deps2));
    }
}
