//! Cached environment creation for inline dependencies.
//!
//! Delegates to `kernel_env` for the actual environment creation while
//! providing a [`BroadcastProgressHandler`] that forwards progress events
//! to connected notebook clients via the broadcast channel.

use std::collections::HashSet;
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
        requires_python: Some(">=3.13".to_string()),
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

/// Result of comparing inline deps against pool packages.
#[derive(Debug)]
pub enum PoolDepRelation {
    /// All inline deps are already installed in the pool env.
    Subset,
    /// Pool covers some deps; these extras need installing.
    Additive { delta: Vec<String> },
    /// Cannot determine compatibility (version pins, etc.) — build from scratch.
    Independent,
}

/// Extract the bare package name from a dependency specifier.
///
/// Returns `None` if the dep has a version constraint (anything beyond a bare name),
/// since we can't guarantee the pool's installed version satisfies it.
fn bare_package_name(dep: &str) -> Option<&str> {
    let trimmed = dep.trim();
    if trimmed.is_empty() {
        return None;
    }
    // If the dep contains any version specifier characters, it's not bare.
    // This is conservative: "pandas>=2.0" → None, "pandas" → Some("pandas")
    let specifier_chars = ['>', '<', '=', '!', '~', '[', ';', '@'];
    if trimmed.contains(|c: char| specifier_chars.contains(&c) || c.is_whitespace()) {
        return None;
    }
    Some(trimmed)
}

/// Normalize a package name for comparison: lowercase, replace `_` with `-`.
fn normalize_package_name(name: &str) -> String {
    name.to_lowercase().replace('_', "-")
}

/// Compare inline deps against pool prewarmed packages.
///
/// Conservative approach: only bare package names (no version specifiers)
/// are eligible for pool reuse. If any dep has a version constraint,
/// returns `Independent`.
pub fn compare_deps_to_pool(inline_deps: &[String], pool_packages: &[String]) -> PoolDepRelation {
    if inline_deps.is_empty() {
        return PoolDepRelation::Subset;
    }

    let pool_normalized: HashSet<String> = pool_packages
        .iter()
        .map(|p| normalize_package_name(p))
        .collect();

    let mut delta = Vec::new();

    for dep in inline_deps {
        let Some(bare) = bare_package_name(dep) else {
            // Has a version specifier — can't guarantee pool compatibility
            return PoolDepRelation::Independent;
        };

        let normalized = normalize_package_name(bare);
        if !pool_normalized.contains(&normalized) {
            delta.push(dep.clone());
        }
    }

    if delta.is_empty() {
        PoolDepRelation::Subset
    } else {
        PoolDepRelation::Additive { delta }
    }
}

/// Check if a cached UV inline environment already exists for the given deps.
///
/// Returns `Some(PreparedEnv)` on cache hit, `None` on miss.
pub fn check_uv_inline_cache(deps: &[String], prerelease: Option<&str>) -> Option<PreparedEnv> {
    let uv_deps = kernel_env::UvDependencies {
        dependencies: deps.to_vec(),
        requires_python: Some(">=3.13".to_string()),
        prerelease: prerelease.map(|s| s.to_string()),
    };

    let hash = kernel_env::uv::compute_env_hash(&uv_deps, None);
    let cache_dir = get_inline_cache_dir();
    let venv_path = cache_dir.join(&hash);

    #[cfg(unix)]
    let python_path = venv_path.join("bin").join("python");
    #[cfg(windows)]
    let python_path = venv_path.join("Scripts").join("python.exe");

    if python_path.exists() {
        Some(PreparedEnv {
            env_path: venv_path,
            python_path,
        })
    } else {
        None
    }
}

/// Check if a cached Conda inline environment already exists for the given deps.
///
/// Returns `Some(PreparedEnv)` on cache hit, `None` on miss.
pub fn check_conda_inline_cache(deps: &[String], channels: &[String]) -> Option<PreparedEnv> {
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

    let hash = kernel_env::conda::compute_env_hash(&conda_deps);
    let cache_dir = get_inline_cache_dir();
    let env_path = cache_dir.join(&hash);

    #[cfg(unix)]
    let python_path = env_path.join("bin").join("python");
    #[cfg(windows)]
    let python_path = env_path.join("Scripts").join("python.exe");

    if python_path.exists() {
        Some(PreparedEnv {
            env_path,
            python_path,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_package_name() {
        assert_eq!(bare_package_name("pandas"), Some("pandas"));
        assert_eq!(bare_package_name("numpy"), Some("numpy"));
        assert_eq!(bare_package_name("  pandas  "), Some("pandas"));
        assert_eq!(bare_package_name(""), None);

        // Version specifiers → None
        assert_eq!(bare_package_name("pandas>=2.0"), None);
        assert_eq!(bare_package_name("pandas==2.0.0"), None);
        assert_eq!(bare_package_name("pandas<3"), None);
        assert_eq!(bare_package_name("pandas~=2.0"), None);
        assert_eq!(bare_package_name("pandas!=1.0"), None);

        // Extras / markers → None
        assert_eq!(bare_package_name("pandas[sql]"), None);
        assert_eq!(bare_package_name("pandas ; python_version >= '3.8'"), None);
        assert_eq!(bare_package_name("pandas @ https://example.com"), None);
    }

    #[test]
    fn test_normalize_package_name() {
        assert_eq!(normalize_package_name("Pandas"), "pandas");
        assert_eq!(normalize_package_name("scikit_learn"), "scikit-learn");
        assert_eq!(normalize_package_name("PyArrow"), "pyarrow");
    }

    #[test]
    fn test_compare_subset() {
        let pool = vec![
            "ipykernel".into(),
            "pandas".into(),
            "numpy".into(),
            "matplotlib".into(),
        ];
        let deps = vec!["pandas".into(), "numpy".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_subset_case_insensitive() {
        let pool = vec!["ipykernel".into(), "PyArrow".into()];
        let deps = vec!["pyarrow".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_additive() {
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas".into(), "scikit-learn".into()];
        match compare_deps_to_pool(&deps, &pool) {
            PoolDepRelation::Additive { delta } => {
                assert_eq!(delta, vec!["scikit-learn".to_string()]);
            }
            other => panic!("expected Additive, got {:?}", other),
        }
    }

    #[test]
    fn test_compare_independent_version_pin() {
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas==2.0.0".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_empty_deps() {
        let pool = vec!["ipykernel".into()];
        assert!(matches!(
            compare_deps_to_pool(&[], &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_underscore_normalization() {
        let pool = vec!["scikit-learn".into()];
        let deps = vec!["scikit_learn".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }
}
