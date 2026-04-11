//! UV-based virtual environment management.
//!
//! Creates, caches, and prewarms UV virtual environments for Jupyter kernels.
//! Environments are keyed by a SHA-256 hash of (dependencies + requires-python
//! + env_id) and stored under the cache directory. UV is auto-bootstrapped via
//!   rattler if not found on PATH.

use anyhow::{anyhow, Result};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::progress::{EnvProgressPhase, ProgressHandler};

/// UV dependency specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UvDependencies {
    pub dependencies: Vec<String>,
    #[serde(rename = "requires-python")]
    pub requires_python: Option<String>,
    /// UV prerelease strategy. When set, passes `--prerelease <value>` to uv pip install.
    /// Possible values: "disallow", "allow", "if-necessary", "explicit", "if-necessary-or-explicit"
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prerelease: Option<String>,
}

/// A resolved UV virtual environment on disk.
#[derive(Debug)]
pub struct UvEnvironment {
    pub venv_path: PathBuf,
    pub python_path: PathBuf,
}

/// Get the default cache directory for UV environments.
pub fn default_cache_dir_uv() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("envs")
}

/// Check if uv is available (either on PATH or bootstrappable via rattler).
pub async fn check_uv_available() -> bool {
    kernel_launch::tools::get_uv_path().await.is_ok()
}

/// Compute a stable cache key for the given dependencies.
///
/// When deps are empty and env_id is provided, includes env_id in hash
/// for per-notebook isolation.
pub fn compute_env_hash(deps: &UvDependencies, env_id: Option<&str>) -> String {
    let mut hasher = Sha256::new();

    let mut sorted_deps = deps.dependencies.clone();
    sorted_deps.sort();

    // For empty deps, include env_id for per-notebook isolation
    if sorted_deps.is_empty() {
        if let Some(id) = env_id {
            hasher.update(b"env_id:");
            hasher.update(id.as_bytes());
            hasher.update(b"\n");
        }
    }

    for dep in &sorted_deps {
        hasher.update(dep.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(ref py) = deps.requires_python {
        hasher.update(b"requires-python:");
        hasher.update(py.as_bytes());
        // NOTE: No trailing newline here to maintain backward-compatible hashes
        // for existing environments that don't have prerelease set.
    }

    if let Some(ref prerelease) = deps.prerelease {
        // Add separator before prerelease to distinguish from requires-python value
        hasher.update(b"\nprerelease:");
        hasher.update(prerelease.as_bytes());
    }

    let hash = hasher.finalize();
    hex::encode(hash)[..16].to_string()
}

/// Prepare a virtual environment with the given dependencies.
///
/// Uses cached environments when possible (keyed by dependency hash).
/// If the cache doesn't exist, creates a new environment with
/// `uv venv` + `uv pip install`.
///
/// The `env_id` parameter enables per-notebook isolation for empty deps:
/// - If deps are empty and env_id is provided, the env is unique to that notebook
/// - If deps are non-empty, env_id is ignored and envs are shared by dep hash
pub async fn prepare_environment(
    deps: &UvDependencies,
    env_id: Option<&str>,
    handler: Arc<dyn ProgressHandler>,
) -> Result<UvEnvironment> {
    prepare_environment_in(deps, env_id, &default_cache_dir_uv(), handler).await
}

/// Like [`prepare_environment`] but with an explicit cache directory.
pub async fn prepare_environment_in(
    deps: &UvDependencies,
    env_id: Option<&str>,
    cache_dir: &Path,
    handler: Arc<dyn ProgressHandler>,
) -> Result<UvEnvironment> {
    let hash = compute_env_hash(deps, env_id);
    let venv_path = cache_dir.join(&hash);

    handler.on_progress(
        "uv",
        EnvProgressPhase::Starting {
            env_hash: hash.clone(),
        },
    );

    #[cfg(target_os = "windows")]
    let python_path = venv_path.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = venv_path.join("bin").join("python");

    // Cache hit
    if venv_path.exists() && python_path.exists() {
        info!("Using cached environment at {:?}", venv_path);
        crate::gc::touch_last_used(&venv_path).await;
        handler.on_progress(
            "uv",
            EnvProgressPhase::CacheHit {
                env_path: venv_path.to_string_lossy().to_string(),
            },
        );
        handler.on_progress(
            "uv",
            EnvProgressPhase::Ready {
                env_path: venv_path.to_string_lossy().to_string(),
                python_path: python_path.to_string_lossy().to_string(),
            },
        );
        return Ok(UvEnvironment {
            venv_path,
            python_path,
        });
    }

    info!("Creating new environment at {:?}", venv_path);

    let uv_path = kernel_launch::tools::get_uv_path().await?;

    tokio::fs::create_dir_all(cache_dir).await?;

    // Remove partial environment
    if venv_path.exists() {
        tokio::fs::remove_dir_all(&venv_path).await?;
    }

    // Create venv
    handler.on_progress("uv", EnvProgressPhase::CreatingVenv);

    let mut venv_cmd = tokio::process::Command::new(&uv_path);
    venv_cmd.arg("venv").arg(&venv_path);

    if let Some(ref py_version) = deps.requires_python {
        let version = py_version
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .to_string();
        if !version.is_empty() {
            venv_cmd.arg("--python").arg(&version);
        }
    }

    let venv_output = venv_cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !venv_output.status.success() {
        let stderr = String::from_utf8_lossy(&venv_output.stderr);
        let error_msg = format!("Failed to create virtual environment: {}", stderr);
        handler.on_progress(
            "uv",
            EnvProgressPhase::Error {
                message: error_msg.clone(),
            },
        );
        return Err(anyhow!(error_msg));
    }

    // Build list of packages to install (for progress reporting)
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
        "uv".to_string(), // For %uv magic in notebooks
    ];
    packages.extend(deps.dependencies.iter().cloned());

    // Build install command args.
    // Use hardlink mode to share files from uv's global cache,
    // dramatically reducing per-env disk usage. uv falls back to
    // copies automatically if hardlinks aren't supported.
    let mut install_args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--link-mode".to_string(),
        "hardlink".to_string(),
        "--python".to_string(),
        python_path.to_string_lossy().to_string(),
    ];

    // Add prerelease flag if set
    if let Some(ref prerelease) = deps.prerelease {
        install_args.push("--prerelease".to_string());
        install_args.push(prerelease.clone());
    }

    install_args.extend(packages.iter().cloned());

    handler.on_progress("uv", EnvProgressPhase::InstallingPackages { packages });

    // Try offline first: use local cache if available
    let mut offline_args = install_args.clone();
    // Insert --offline after "pip install" (index 2)
    offline_args.insert(2, "--offline".to_string());

    let offline_output = tokio::process::Command::new(&uv_path)
        .args(&offline_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if offline_output.status.success() {
        info!("Resolved dependencies from local cache (offline mode)");
        handler.on_progress("uv", EnvProgressPhase::OfflineHit);

        // Success path: environment is ready
        info!("Environment ready at {:?}", venv_path);
        crate::gc::touch_last_used(&venv_path).await;
        handler.on_progress(
            "uv",
            EnvProgressPhase::Ready {
                env_path: venv_path.to_string_lossy().to_string(),
                python_path: python_path.to_string_lossy().to_string(),
            },
        );

        return Ok(UvEnvironment {
            venv_path,
            python_path,
        });
    }

    // Offline failed, fall back to network install
    debug!(
        "Offline install failed (expected if packages not cached): {}",
        String::from_utf8_lossy(&offline_output.stderr)
    );

    let install_output = tokio::process::Command::new(&uv_path)
        .args(&install_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    // If install failed, retry once with --refresh to bypass stale index cache.
    // This handles cases where a recently-published version (e.g. a nightly pre-release)
    // isn't found because uv's cached package index is stale.
    let install_output = if !install_output.status.success() {
        let first_stderr = String::from_utf8_lossy(&install_output.stderr);
        info!(
            "uv pip install failed, retrying with --refresh. First attempt stderr: {}",
            first_stderr
        );
        let mut retry_args = install_args.clone();
        // Insert --refresh after "pip install" (index 2), before --link-mode
        retry_args.insert(2, "--refresh".to_string());
        tokio::process::Command::new(&uv_path)
            .args(&retry_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?
    } else {
        install_output
    };

    if !install_output.status.success() {
        tokio::fs::remove_dir_all(&venv_path).await.ok();
        let stderr = String::from_utf8_lossy(&install_output.stderr);
        let error_msg = format!("Failed to install dependencies: {}", stderr);
        handler.on_progress(
            "uv",
            EnvProgressPhase::Error {
                message: error_msg.clone(),
            },
        );
        return Err(anyhow!(error_msg));
    }

    info!("Environment ready at {:?}", venv_path);
    crate::gc::touch_last_used(&venv_path).await;
    handler.on_progress(
        "uv",
        EnvProgressPhase::Ready {
            env_path: venv_path.to_string_lossy().to_string(),
            python_path: python_path.to_string_lossy().to_string(),
        },
    );

    Ok(UvEnvironment {
        venv_path,
        python_path,
    })
}

/// Install additional dependencies into an existing environment.
pub async fn sync_dependencies(env: &UvEnvironment, deps: &[String]) -> Result<()> {
    if deps.is_empty() {
        return Ok(());
    }

    info!("Syncing {} dependencies to {:?}", deps.len(), env.venv_path);

    let uv_path = kernel_launch::tools::get_uv_path().await?;

    let mut install_args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--link-mode".to_string(),
        "hardlink".to_string(),
        "--python".to_string(),
        env.python_path.to_string_lossy().to_string(),
    ];

    for dep in deps {
        install_args.push(dep.clone());
    }

    // Try offline first
    let mut offline_args = install_args.clone();
    // Insert --offline after "pip install" (index 2)
    offline_args.insert(2, "--offline".to_string());

    let offline_output = tokio::process::Command::new(&uv_path)
        .args(&offline_args)
        .output()
        .await?;

    if offline_output.status.success() {
        info!("Synced dependencies from local cache (offline mode)");
        return Ok(());
    }

    // Offline failed, fall back to network install
    debug!(
        "Offline sync failed (expected if packages not cached): {}",
        String::from_utf8_lossy(&offline_output.stderr)
    );

    let output = tokio::process::Command::new(&uv_path)
        .args(&install_args)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to sync dependencies: {}", stderr));
    }

    info!("Dependencies synced successfully");
    Ok(())
}

/// Create a prewarmed environment with ipykernel, ipywidgets, and
/// any caller-supplied extra packages.
///
/// Returns an environment at `prewarm-{uuid}` that can later be claimed
/// via [`claim_prewarmed_environment`].
pub async fn create_prewarmed_environment(
    extra_packages: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<UvEnvironment> {
    create_prewarmed_environment_in(&default_cache_dir_uv(), extra_packages, handler).await
}

/// Like [`create_prewarmed_environment`] but with an explicit cache directory.
pub async fn create_prewarmed_environment_in(
    cache_dir: &Path,
    extra_packages: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<UvEnvironment> {
    let temp_id = format!("prewarm-{}", uuid::Uuid::new_v4());
    let venv_path = cache_dir.join(&temp_id);

    #[cfg(target_os = "windows")]
    let python_path = venv_path.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = venv_path.join("bin").join("python");

    info!(
        "[prewarm] Creating prewarmed environment at {:?}",
        venv_path
    );

    let uv_path = kernel_launch::tools::get_uv_path().await?;

    tokio::fs::create_dir_all(cache_dir).await?;

    handler.on_progress("uv", EnvProgressPhase::CreatingVenv);

    let venv_output = tokio::process::Command::new(&uv_path)
        .arg("venv")
        .arg(&venv_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !venv_output.status.success() {
        let stderr = String::from_utf8_lossy(&venv_output.stderr);
        return Err(anyhow!(
            "Failed to create prewarmed virtual environment: {}",
            stderr
        ));
    }

    // Install ipykernel, ipywidgets, uv, and any extra packages.
    // Use hardlink mode to share files from uv's global cache.
    let mut install_args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--link-mode".to_string(),
        "hardlink".to_string(),
        "--python".to_string(),
        python_path.to_string_lossy().to_string(),
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "uv".to_string(), // For %uv magic in notebooks
    ];
    if !extra_packages.is_empty() {
        info!("[prewarm] Including extra packages: {:?}", extra_packages);
        install_args.extend(extra_packages.iter().cloned());
    }

    handler.on_progress(
        "uv",
        EnvProgressPhase::InstallingPackages {
            packages: install_args[6..].to_vec(),
        },
    );

    // Try offline first for prewarmed environments
    let mut offline_args = install_args.clone();
    // Insert --offline after "pip install" (index 2)
    offline_args.insert(2, "--offline".to_string());

    let offline_output = tokio::process::Command::new(&uv_path)
        .args(&offline_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if offline_output.status.success() {
        info!("[prewarm] Resolved dependencies from local cache (offline mode)");
        handler.on_progress("uv", EnvProgressPhase::OfflineHit);

        info!("[prewarm] Prewarmed environment ready at {:?}", venv_path);

        let env = UvEnvironment {
            venv_path,
            python_path,
        };

        warmup_environment(&env).await?;

        handler.on_progress(
            "uv",
            EnvProgressPhase::Ready {
                env_path: env.venv_path.to_string_lossy().to_string(),
                python_path: env.python_path.to_string_lossy().to_string(),
            },
        );

        return Ok(env);
    }

    // Offline failed, fall back to network install
    debug!(
        "[prewarm] Offline install failed (expected if packages not cached): {}",
        String::from_utf8_lossy(&offline_output.stderr)
    );

    let install_output = tokio::process::Command::new(&uv_path)
        .args(&install_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !install_output.status.success() {
        tokio::fs::remove_dir_all(&venv_path).await.ok();
        let stderr = String::from_utf8_lossy(&install_output.stderr);
        return Err(anyhow!(
            "Failed to install ipykernel in prewarmed environment: {}",
            stderr
        ));
    }

    info!("[prewarm] Prewarmed environment ready at {:?}", venv_path);

    let env = UvEnvironment {
        venv_path,
        python_path,
    };

    warmup_environment(&env).await?;

    handler.on_progress(
        "uv",
        EnvProgressPhase::Ready {
            env_path: env.venv_path.to_string_lossy().to_string(),
            python_path: env.python_path.to_string_lossy().to_string(),
        },
    );

    Ok(env)
}

/// Claim a prewarmed environment for a specific notebook.
///
/// Moves the prewarmed environment to the correct cache location based
/// on `env_id`, so it will be found by [`prepare_environment`] later.
pub async fn claim_prewarmed_environment(
    prewarmed: UvEnvironment,
    env_id: &str,
) -> Result<UvEnvironment> {
    claim_prewarmed_environment_in(prewarmed, env_id, &default_cache_dir_uv()).await
}

/// Like [`claim_prewarmed_environment`] but with an explicit cache directory.
pub async fn claim_prewarmed_environment_in(
    prewarmed: UvEnvironment,
    env_id: &str,
    cache_dir: &Path,
) -> Result<UvEnvironment> {
    let deps = UvDependencies {
        dependencies: vec![],
        requires_python: None,
        prerelease: None,
    };
    let hash = compute_env_hash(&deps, Some(env_id));
    let dest_path = cache_dir.join(&hash);

    #[cfg(target_os = "windows")]
    let python_path = dest_path.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = dest_path.join("bin").join("python");

    if dest_path.exists() {
        info!(
            "[prewarm] Destination already exists, removing prewarmed env at {:?}",
            prewarmed.venv_path
        );
        tokio::fs::remove_dir_all(&prewarmed.venv_path).await.ok();
        return Ok(UvEnvironment {
            venv_path: dest_path,
            python_path,
        });
    }

    info!(
        "[prewarm] Claiming prewarmed environment: {:?} -> {:?}",
        prewarmed.venv_path, dest_path
    );

    match tokio::fs::rename(&prewarmed.venv_path, &dest_path).await {
        Ok(()) => {
            info!("[prewarm] Environment claimed via rename");
        }
        Err(e) => {
            info!("[prewarm] Rename failed ({}), falling back to copy", e);
            copy_dir_recursive(&prewarmed.venv_path, &dest_path).await?;
            tokio::fs::remove_dir_all(&prewarmed.venv_path).await.ok();
            info!("[prewarm] Environment claimed via copy");
        }
    }

    Ok(UvEnvironment {
        venv_path: dest_path,
        python_path,
    })
}

/// Find existing prewarmed environments from previous sessions.
pub async fn find_existing_prewarmed_environments() -> Vec<UvEnvironment> {
    find_existing_prewarmed_environments_in(&default_cache_dir_uv()).await
}

/// Like [`find_existing_prewarmed_environments`] but with an explicit cache directory.
pub async fn find_existing_prewarmed_environments_in(cache_dir: &Path) -> Vec<UvEnvironment> {
    let mut found = Vec::new();

    let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await else {
        return found;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("prewarm-") {
            continue;
        }

        let venv_path = entry.path();

        #[cfg(target_os = "windows")]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = venv_path.join("bin").join("python");

        if !python_path.exists() {
            info!(
                "[prewarm] Removing invalid prewarmed env (no python): {:?}",
                venv_path
            );
            tokio::fs::remove_dir_all(&venv_path).await.ok();
            continue;
        }

        info!(
            "[prewarm] Found existing prewarmed environment: {:?}",
            venv_path
        );
        found.push(UvEnvironment {
            venv_path,
            python_path,
        });
    }

    found
}

/// Warm up a UV environment by running Python to trigger .pyc compilation.
pub async fn warmup_environment(env: &UvEnvironment) -> Result<()> {
    let warmup_start = std::time::Instant::now();
    info!("[prewarm] Warming up UV environment at {:?}", env.venv_path);

    let site_packages = find_site_packages(&env.venv_path);
    let warmup_script = crate::warmup::build_warmup_command(&[], true, site_packages.as_deref());

    let output = tokio::process::Command::new(&env.python_path)
        .args(["-c", &warmup_script])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!(
            "[prewarm] UV warmup failed for {:?}: {}",
            env.venv_path,
            stderr
        );
        return Ok(());
    }

    let marker_path = env.venv_path.join(".warmed");
    tokio::fs::write(&marker_path, "").await.ok();

    info!(
        "[prewarm] UV warmup complete for {:?} in {}ms",
        env.venv_path,
        warmup_start.elapsed().as_millis()
    );

    Ok(())
}

/// Check if a UV environment has been warmed up.
pub fn is_environment_warmed(env: &UvEnvironment) -> bool {
    env.venv_path.join(".warmed").exists()
}

/// Copy an existing UV environment to a new location.
pub async fn copy_environment(source: &UvEnvironment, new_env_id: &str) -> Result<UvEnvironment> {
    let cache_dir = default_cache_dir_uv();
    let dest_path = cache_dir.join(new_env_id);

    if dest_path.exists() {
        info!("Clone environment already exists at {:?}", dest_path);
        #[cfg(target_os = "windows")]
        let python_path = dest_path.join("Scripts").join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = dest_path.join("bin").join("python");

        return Ok(UvEnvironment {
            venv_path: dest_path,
            python_path,
        });
    }

    info!(
        "Copying environment from {:?} to {:?}",
        source.venv_path, dest_path
    );

    copy_dir_recursive(&source.venv_path, &dest_path).await?;

    #[cfg(target_os = "windows")]
    let python_path = dest_path.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = dest_path.join("bin").join("python");

    info!("Environment copied successfully");

    Ok(UvEnvironment {
        venv_path: dest_path,
        python_path,
    })
}

/// No-op cleanup (cached environments are kept for reuse).
pub async fn cleanup_environment(_env: &UvEnvironment) -> Result<()> {
    Ok(())
}

/// Force remove a cached environment.
#[allow(dead_code)]
pub async fn remove_environment(env: &UvEnvironment) -> Result<()> {
    if env.venv_path.exists() {
        tokio::fs::remove_dir_all(&env.venv_path).await?;
    }
    Ok(())
}

/// Clear all cached UV environments.
#[allow(dead_code)]
pub async fn clear_cache() -> Result<()> {
    let cache_dir = default_cache_dir_uv();
    if cache_dir.exists() {
        tokio::fs::remove_dir_all(&cache_dir).await?;
    }
    Ok(())
}

/// Recursively copy a directory, preserving symlinks.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;

    while let Some(entry) = entries.next_entry().await? {
        let ty = entry.file_type().await?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let link_target = tokio::fs::read_link(&src_path).await?;
                tokio::fs::symlink(&link_target, &dst_path).await?;
            }
            #[cfg(windows)]
            tokio::fs::copy(&src_path, &dst_path).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path).await?;
        }
    }

    Ok(())
}

/// Find the site-packages directory inside a venv/env.
fn find_site_packages(base_path: &std::path::Path) -> Option<String> {
    let lib_dir = base_path.join("lib");
    if let Ok(entries) = std::fs::read_dir(&lib_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("python") {
                        let sp = path.join("site-packages");
                        if sp.is_dir() {
                            return sp.to_str().map(String::from);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_env_hash_stable() {
        let deps = UvDependencies {
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
        let deps1 = UvDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let deps2 = UvDependencies {
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
        let deps1 = UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let deps2 = UvDependencies {
            dependencies: vec!["numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };

        assert_ne!(
            compute_env_hash(&deps1, None),
            compute_env_hash(&deps2, None)
        );
    }

    #[test]
    fn test_compute_env_hash_env_id_isolation() {
        let deps = UvDependencies {
            dependencies: vec![],
            requires_python: None,
            prerelease: None,
        };

        let hash1 = compute_env_hash(&deps, Some("notebook-1"));
        let hash2 = compute_env_hash(&deps, Some("notebook-2"));
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_env_hash_env_id_ignored_with_deps() {
        let deps = UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let hash1 = compute_env_hash(&deps, Some("notebook-1"));
        let hash2 = compute_env_hash(&deps, Some("notebook-2"));
        // env_id is only included for empty deps
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_env_hash_prerelease_changes_hash() {
        let deps1 = UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };

        let deps2 = UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: Some("allow".to_string()),
        };

        assert_ne!(
            compute_env_hash(&deps1, None),
            compute_env_hash(&deps2, None)
        );
    }

    #[test]
    fn test_compute_env_hash_prerelease_stable() {
        let deps = UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: Some("allow".to_string()),
        };

        let hash1 = compute_env_hash(&deps, None);
        let hash2 = compute_env_hash(&deps, None);
        assert_eq!(hash1, hash2);
    }
}
