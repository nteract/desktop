//! Cached environment creation for inline dependencies.
//!
//! Delegates to `kernel_env` for the actual environment creation while
//! providing a [`BroadcastProgressHandler`] that forwards progress events
//! to connected notebook clients via the broadcast channel.

use std::sync::Arc;

use anyhow::Result;
use kernel_env::progress::{EnvProgressPhase, ProgressHandler};
use tokio::sync::broadcast;

use crate::protocol::NotebookBroadcast;

// Re-export the PreparedEnv-equivalent types for callers that still
// use the old `inline_env::PreparedEnv` pattern.
pub use kernel_env::conda::CondaEnvironment;
pub use kernel_env::uv::UvEnvironment;

/// Result of preparing an environment with inline deps.
#[derive(Debug, Clone)]
pub struct PreparedEnv {
    pub env_path: std::path::PathBuf,
    pub python_path: std::path::PathBuf,
}

/// Progress handler that broadcasts [`EnvProgressPhase`] events to all
/// connected notebook clients via a [`broadcast::Sender`].
pub struct BroadcastProgressHandler {
    tx: broadcast::Sender<NotebookBroadcast>,
}

impl BroadcastProgressHandler {
    pub fn new(tx: broadcast::Sender<NotebookBroadcast>) -> Self {
        Self { tx }
    }
}

impl ProgressHandler for BroadcastProgressHandler {
    fn on_progress(&self, env_type: &str, phase: EnvProgressPhase) {
        // Log all phases
        kernel_env::LogHandler.on_progress(env_type, phase.clone());

        // Broadcast to connected clients
        let _ = self.tx.send(NotebookBroadcast::EnvProgress {
            env_type: env_type.to_string(),
            phase,
        });
    }
}

/// Get the cache directory for inline dependency environments.
fn get_inline_cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("runt")
        .join("inline-envs")
}

/// Prepare a cached UV environment with the given inline dependencies.
///
/// If a cached environment with the same deps already exists, returns it
/// immediately. Otherwise creates a new environment with uv venv + uv pip install.
pub async fn prepare_uv_inline_env(
    deps: &[String],
    prerelease: Option<&str>,
    handler: Arc<dyn ProgressHandler>,
) -> Result<PreparedEnv> {
    let uv_deps = kernel_env::UvDependencies {
        dependencies: deps.to_vec(),
        requires_python: None,
        prerelease: prerelease.map(|s| s.to_string()),
    };

    let env =
        kernel_env::uv::prepare_environment_in(&uv_deps, None, &get_inline_cache_dir(), handler)
            .await?;

    Ok(PreparedEnv {
        env_path: env.venv_path,
        python_path: env.python_path,
    })
}

/// Prepare a cached Conda environment with the given inline dependencies.
///
/// If a cached environment with the same deps+channels already exists, returns
/// it immediately. Otherwise creates a new environment using rattler.
pub async fn prepare_conda_inline_env(
    deps: &[String],
    channels: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<PreparedEnv> {
    let conda_deps = kernel_env::CondaDependencies {
        dependencies: deps.to_vec(),
        channels: if channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            channels.to_vec()
        },
        python: None,
        env_id: None,
    };

    let env =
        kernel_env::conda::prepare_environment_in(&conda_deps, &get_inline_cache_dir(), handler)
            .await?;

    Ok(PreparedEnv {
        env_path: env.env_path,
        python_path: env.python_path,
    })
}
