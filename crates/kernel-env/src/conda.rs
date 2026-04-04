//! Conda environment management via rattler.
//!
//! Creates, caches, and prewarms conda environments for Jupyter kernels.
//! Environments are keyed by a SHA-256 hash of (dependencies + channels +
//! python constraint + env_id) and stored under the cache directory.

use anyhow::{anyhow, Result};
use log::{info, warn};
use rattler::{default_cache_dir, install::Installer, package_cache::PackageCache};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions, Platform,
    PrefixRecord,
};
use rattler_repodata_gateway::Gateway;
use rattler_solve::{resolvo, SolverImpl, SolverTask};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::progress::{EnvProgressPhase, ProgressHandler, RattlerReporter};

/// Conda dependency specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CondaDependencies {
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub python: Option<String>,
    /// Unique environment ID for per-notebook isolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
}

/// A resolved conda environment on disk.
#[derive(Debug, Clone)]
pub struct CondaEnvironment {
    pub env_path: PathBuf,
    pub python_path: PathBuf,
}

/// Get the default cache directory for conda environments.
pub fn default_cache_dir_conda() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("conda-envs")
}

/// Compute a stable cache key for the given dependencies.
///
/// The hash includes sorted deps, sorted channels, python constraint,
/// and env_id (for per-notebook isolation).
pub fn compute_env_hash(deps: &CondaDependencies) -> String {
    let mut hasher = Sha256::new();

    let mut sorted_deps = deps.dependencies.clone();
    sorted_deps.sort();
    for dep in &sorted_deps {
        hasher.update(dep.as_bytes());
        hasher.update(b"\n");
    }

    let mut sorted_channels = deps.channels.clone();
    sorted_channels.sort();
    for channel in &sorted_channels {
        hasher.update(b"channel:");
        hasher.update(channel.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(ref py) = deps.python {
        hasher.update(b"python:");
        hasher.update(py.as_bytes());
    }

    if let Some(ref env_id) = deps.env_id {
        hasher.update(b"env_id:");
        hasher.update(env_id.as_bytes());
    }

    let hash = hasher.finalize();
    format!("{:x}", hash)[..16].to_string()
}

/// Prepare a conda environment with the given dependencies.
///
/// Uses cached environments when possible (keyed by dependency hash).
/// If the cache doesn't exist, creates a new environment using rattler
/// (repodata fetch → solve → download → install).
///
/// Progress events are emitted via `handler` throughout the lifecycle.
pub async fn prepare_environment(
    deps: &CondaDependencies,
    handler: Arc<dyn ProgressHandler>,
) -> Result<CondaEnvironment> {
    prepare_environment_in(deps, &default_cache_dir_conda(), handler).await
}

/// Like [`prepare_environment`] but with an explicit cache directory.
pub async fn prepare_environment_in(
    deps: &CondaDependencies,
    cache_dir: &Path,
    handler: Arc<dyn ProgressHandler>,
) -> Result<CondaEnvironment> {
    let hash = compute_env_hash(deps);
    let env_path = cache_dir.join(&hash);

    handler.on_progress(
        "conda",
        EnvProgressPhase::Starting {
            env_hash: hash.clone(),
        },
    );

    #[cfg(target_os = "windows")]
    let python_path = env_path.join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = env_path.join("bin").join("python");

    // Cache hit
    if env_path.exists() && python_path.exists() {
        info!("Using cached conda environment at {:?}", env_path);
        crate::gc::touch_last_used(&env_path).await;
        handler.on_progress(
            "conda",
            EnvProgressPhase::CacheHit {
                env_path: env_path.to_string_lossy().to_string(),
            },
        );
        handler.on_progress(
            "conda",
            EnvProgressPhase::Ready {
                env_path: env_path.to_string_lossy().to_string(),
                python_path: python_path.to_string_lossy().to_string(),
            },
        );
        return Ok(CondaEnvironment {
            env_path,
            python_path,
        });
    }

    info!("Creating new conda environment at {:?}", env_path);

    tokio::fs::create_dir_all(cache_dir).await?;

    // Remove partial environment
    if env_path.exists() {
        tokio::fs::remove_dir_all(&env_path).await?;
    }

    install_conda_env(&env_path, deps, handler.clone()).await?;

    // Verify python exists
    if !python_path.exists() {
        tokio::fs::remove_dir_all(&env_path).await.ok();
        return Err(anyhow!(
            "Python not found at {:?} after conda install",
            python_path
        ));
    }

    crate::gc::touch_last_used(&env_path).await;
    handler.on_progress(
        "conda",
        EnvProgressPhase::Ready {
            env_path: env_path.to_string_lossy().to_string(),
            python_path: python_path.to_string_lossy().to_string(),
        },
    );

    Ok(CondaEnvironment {
        env_path,
        python_path,
    })
}

/// Core rattler solve + install logic, extracted for reuse by prepare and prewarm.
async fn install_conda_env(
    env_path: &Path,
    deps: &CondaDependencies,
    handler: Arc<dyn ProgressHandler>,
) -> Result<()> {
    let cache_dir = env_path
        .parent()
        .unwrap_or_else(|| Path::new("/tmp"))
        .to_path_buf();
    let channel_config = ChannelConfig::default_with_root_dir(cache_dir);

    // Parse channels
    let channels: Vec<Channel> = if deps.channels.is_empty() {
        vec![Channel::from_str("conda-forge", &channel_config)?]
    } else {
        deps.channels
            .iter()
            .map(|c| Channel::from_str(c, &channel_config))
            .collect::<std::result::Result<Vec<_>, _>>()?
    };

    let channel_names: Vec<String> = channels.iter().map(|c| c.name().to_string()).collect();

    handler.on_progress(
        "conda",
        EnvProgressPhase::FetchingRepodata {
            channels: channel_names,
        },
    );

    // Build specs
    let match_spec_options = ParseMatchSpecOptions::strict();
    let mut specs: Vec<MatchSpec> = Vec::new();

    if let Some(ref py) = deps.python {
        specs.push(MatchSpec::from_str(
            &format!("python={}", py),
            match_spec_options,
        )?);
    } else {
        specs.push(MatchSpec::from_str("python>=3.13", match_spec_options)?);
    }

    specs.push(MatchSpec::from_str("ipykernel", match_spec_options)?);
    specs.push(MatchSpec::from_str("ipywidgets", match_spec_options)?);
    specs.push(MatchSpec::from_str("anywidget", match_spec_options)?);
    specs.push(MatchSpec::from_str("nbformat", match_spec_options)?);

    for dep in &deps.dependencies {
        if dep != "ipykernel" && dep != "ipywidgets" && dep != "anywidget" && dep != "nbformat" {
            specs.push(MatchSpec::from_str(dep, match_spec_options)?);
        }
    }

    // Rattler cache
    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;
    rattler_cache::ensure_cache_dir(&rattler_cache_dir)
        .map_err(|e| anyhow!("could not create rattler cache directory: {}", e))?;

    // HTTP client
    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    // Gateway
    let gateway = Gateway::builder()
        .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(PackageCache::new(
            rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
        ))
        .with_client(download_client.clone())
        .finish();

    // Query repodata with retry
    let install_platform = Platform::current();
    let platforms = vec![install_platform, Platform::NoArch];

    let repodata_start = Instant::now();
    const MAX_RETRIES: u32 = 3;
    const INITIAL_DELAY_MS: u64 = 1000;

    let mut last_error = None;
    let mut repo_data = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = INITIAL_DELAY_MS * (1 << (attempt - 1));
            info!(
                "Retrying repodata fetch (attempt {}/{}) after {}ms...",
                attempt + 1,
                MAX_RETRIES,
                delay_ms
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        match gateway
            .query(channels.clone(), platforms.clone(), specs.clone())
            .recursive(true)
            .await
        {
            Ok(data) => {
                repo_data = Some(data);
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                let is_retryable = error_str.contains("500")
                    || error_str.contains("502")
                    || error_str.contains("503")
                    || error_str.contains("504")
                    || error_str.contains("timeout")
                    || error_str.contains("connection");

                if is_retryable && attempt < MAX_RETRIES - 1 {
                    info!(
                        "Transient error fetching repodata (attempt {}): {}",
                        attempt + 1,
                        error_str
                    );
                    last_error = Some(e);
                    continue;
                }
                let error_msg = format!("Failed to fetch package metadata: {}", e);
                handler.on_progress(
                    "conda",
                    EnvProgressPhase::Error {
                        message: error_msg.clone(),
                    },
                );
                return Err(anyhow!(error_msg));
            }
        }
    }

    let repo_data = match repo_data {
        Some(data) => data,
        None => {
            let error_msg = format!(
                "Failed to fetch package metadata after {} retries: {}",
                MAX_RETRIES,
                last_error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown error".to_string())
            );
            handler.on_progress(
                "conda",
                EnvProgressPhase::Error {
                    message: error_msg.clone(),
                },
            );
            return Err(anyhow!(error_msg));
        }
    };

    let total_records: usize = repo_data.iter().map(|r| r.len()).sum();
    let repodata_elapsed = repodata_start.elapsed();
    info!(
        "Loaded {} package records in {:?}",
        total_records, repodata_elapsed
    );
    handler.on_progress(
        "conda",
        EnvProgressPhase::RepodataComplete {
            record_count: total_records,
            elapsed_ms: repodata_elapsed.as_millis() as u64,
        },
    );

    // Virtual packages
    let virtual_packages = rattler_virtual_packages::VirtualPackage::detect(
        &rattler_virtual_packages::VirtualPackageOverrides::default(),
    )?
    .iter()
    .map(|vpkg| GenericVirtualPackage::from(vpkg.clone()))
    .collect::<Vec<_>>();

    // Solve
    handler.on_progress(
        "conda",
        EnvProgressPhase::Solving {
            spec_count: specs.len(),
        },
    );

    let solve_start = Instant::now();
    let solver_task = SolverTask {
        virtual_packages,
        specs,
        ..SolverTask::from_iter(&repo_data)
    };

    let solver_result = match resolvo::Solver.solve(solver_task) {
        Ok(result) => result,
        Err(e) => {
            let error_msg = format!("Failed to solve dependencies: {}", e);
            handler.on_progress(
                "conda",
                EnvProgressPhase::Error {
                    message: error_msg.clone(),
                },
            );
            return Err(anyhow!(error_msg));
        }
    };
    let required_packages = solver_result.records;
    let solve_elapsed = solve_start.elapsed();

    info!(
        "Solved: {} packages to install in {:?}",
        required_packages.len(),
        solve_elapsed
    );
    handler.on_progress(
        "conda",
        EnvProgressPhase::SolveComplete {
            package_count: required_packages.len(),
            elapsed_ms: solve_elapsed.as_millis() as u64,
        },
    );

    // Install
    handler.on_progress(
        "conda",
        EnvProgressPhase::Installing {
            total: required_packages.len(),
        },
    );

    let reporter = RattlerReporter::new(handler.clone());
    let install_start = Instant::now();

    match Installer::new()
        .with_download_client(download_client)
        .with_target_platform(install_platform)
        .with_reporter(reporter)
        .install(env_path, required_packages)
        .await
    {
        Ok(_) => {}
        Err(e) => {
            let error_msg = format!("Failed to install packages: {}", e);
            handler.on_progress(
                "conda",
                EnvProgressPhase::Error {
                    message: error_msg.clone(),
                },
            );
            return Err(anyhow!(error_msg));
        }
    }

    let install_elapsed = install_start.elapsed();
    info!(
        "Conda environment ready at {:?} (install took {:?})",
        env_path, install_elapsed
    );
    handler.on_progress(
        "conda",
        EnvProgressPhase::InstallComplete {
            elapsed_ms: install_elapsed.as_millis() as u64,
        },
    );

    Ok(())
}

/// Create a prewarmed conda environment with ipykernel, ipywidgets,
/// and any caller-supplied extra packages.
///
/// Returns an environment at `prewarm-{uuid}` that can later be claimed
/// via [`claim_prewarmed_environment`].
pub async fn create_prewarmed_environment(
    extra_packages: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<CondaEnvironment> {
    create_prewarmed_environment_in(&default_cache_dir_conda(), extra_packages, handler).await
}

/// Like [`create_prewarmed_environment`] but with an explicit cache directory.
pub async fn create_prewarmed_environment_in(
    cache_dir: &Path,
    extra_packages: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<CondaEnvironment> {
    let temp_id = format!("prewarm-{}", uuid::Uuid::new_v4());
    let env_path = cache_dir.join(&temp_id);

    #[cfg(target_os = "windows")]
    let python_path = env_path.join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = env_path.join("bin").join("python");

    info!(
        "[prewarm] Creating prewarmed conda environment at {:?}",
        env_path
    );

    tokio::fs::create_dir_all(cache_dir).await?;

    let mut deps_list = vec!["ipykernel".to_string(), "ipywidgets".to_string()];
    if !extra_packages.is_empty() {
        info!("[prewarm] Including extra packages: {:?}", extra_packages);
        deps_list.extend(extra_packages.iter().cloned());
    }
    let deps = CondaDependencies {
        dependencies: deps_list,
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: None,
    };

    install_conda_env(&env_path, &deps, handler.clone()).await?;

    info!(
        "[prewarm] Prewarmed conda environment created at {:?}",
        env_path
    );

    let env = CondaEnvironment {
        env_path,
        python_path,
    };

    warmup_environment(&env).await?;

    Ok(env)
}

/// Claim a prewarmed environment for a specific notebook.
///
/// Moves the prewarmed environment to the correct cache location based
/// on `env_id`, so it will be found by [`prepare_environment`] later.
pub async fn claim_prewarmed_environment(
    prewarmed: CondaEnvironment,
    env_id: &str,
) -> Result<CondaEnvironment> {
    claim_prewarmed_environment_in(prewarmed, env_id, &default_cache_dir_conda()).await
}

/// Like [`claim_prewarmed_environment`] but with an explicit cache directory.
pub async fn claim_prewarmed_environment_in(
    prewarmed: CondaEnvironment,
    env_id: &str,
    cache_dir: &Path,
) -> Result<CondaEnvironment> {
    let deps = CondaDependencies {
        dependencies: vec!["ipykernel".to_string()],
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: Some(env_id.to_string()),
    };
    let hash = compute_env_hash(&deps);
    let dest_path = cache_dir.join(&hash);

    #[cfg(target_os = "windows")]
    let python_path = dest_path.join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = dest_path.join("bin").join("python");

    if dest_path.exists() {
        info!(
            "[prewarm] Destination already exists, removing prewarmed conda env at {:?}",
            prewarmed.env_path
        );
        tokio::fs::remove_dir_all(&prewarmed.env_path).await.ok();
        return Ok(CondaEnvironment {
            env_path: dest_path,
            python_path,
        });
    }

    info!(
        "[prewarm] Claiming prewarmed conda environment: {:?} -> {:?}",
        prewarmed.env_path, dest_path
    );

    match tokio::fs::rename(&prewarmed.env_path, &dest_path).await {
        Ok(()) => {
            info!("[prewarm] Conda environment claimed via rename");
        }
        Err(e) => {
            info!("[prewarm] Rename failed ({}), falling back to copy", e);
            copy_dir_recursive(&prewarmed.env_path, &dest_path).await?;
            tokio::fs::remove_dir_all(&prewarmed.env_path).await.ok();
            info!("[prewarm] Conda environment claimed via copy");
        }
    }

    Ok(CondaEnvironment {
        env_path: dest_path,
        python_path,
    })
}

/// Find existing prewarmed conda environments from previous sessions.
///
/// Scans the cache directory for `prewarm-*` directories and validates
/// they have a working Python binary.
pub async fn find_existing_prewarmed_environments() -> Vec<CondaEnvironment> {
    find_existing_prewarmed_environments_in(&default_cache_dir_conda()).await
}

/// Like [`find_existing_prewarmed_environments`] but with an explicit cache directory.
pub async fn find_existing_prewarmed_environments_in(cache_dir: &Path) -> Vec<CondaEnvironment> {
    let mut found = Vec::new();

    let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await else {
        return found;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("prewarm-") {
            continue;
        }

        let env_path = entry.path();

        #[cfg(target_os = "windows")]
        let python_path = env_path.join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = env_path.join("bin").join("python");

        if !python_path.exists() {
            info!(
                "[prewarm] Skipping invalid conda env (no python): {:?}",
                env_path
            );
            tokio::fs::remove_dir_all(&env_path).await.ok();
            continue;
        }

        info!(
            "[prewarm] Found existing prewarmed conda environment: {:?}",
            env_path
        );
        found.push(CondaEnvironment {
            env_path,
            python_path,
        });
    }

    found
}

/// Warm up a conda environment by running Python to trigger .pyc compilation.
pub async fn warmup_environment(env: &CondaEnvironment) -> Result<()> {
    let warmup_start = Instant::now();
    info!(
        "[prewarm] Warming up conda environment at {:?}",
        env.env_path
    );

    let warmup_script = r#"
import sys
import ipykernel
import IPython
import ipywidgets
import nbformat
import traitlets
import zmq
from ipykernel.kernelbase import Kernel
from ipykernel.ipkernel import IPythonKernel
from ipykernel.comm import CommManager
print("warmup complete")
"#;

    let output = tokio::process::Command::new(&env.python_path)
        .args(["-c", warmup_script])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("[prewarm] Warmup failed for {:?}: {}", env.env_path, stderr);
        return Ok(());
    }

    let marker_path = env.env_path.join(".warmed");
    tokio::fs::write(&marker_path, "").await.ok();

    info!(
        "[prewarm] Warmup complete for {:?} in {}ms",
        env.env_path,
        warmup_start.elapsed().as_millis()
    );

    Ok(())
}

/// Check if a conda environment has been warmed up.
pub fn is_environment_warmed(env: &CondaEnvironment) -> bool {
    env.env_path.join(".warmed").exists()
}

/// Install additional dependencies into an existing environment.
///
/// Solves and installs new packages into the existing prefix, considering
/// already-installed packages as locked.
pub async fn sync_dependencies(env: &CondaEnvironment, deps: &CondaDependencies) -> Result<()> {
    if deps.dependencies.is_empty() {
        return Ok(());
    }

    info!(
        "Syncing {} dependencies to {:?}",
        deps.dependencies.len(),
        env.env_path
    );

    let cache_dir = env
        .env_path
        .parent()
        .unwrap_or_else(|| Path::new("/tmp"))
        .to_path_buf();
    let channel_config = ChannelConfig::default_with_root_dir(cache_dir);

    let channels: Vec<Channel> = if deps.channels.is_empty() {
        vec![Channel::from_str("conda-forge", &channel_config)?]
    } else {
        deps.channels
            .iter()
            .map(|c| Channel::from_str(c, &channel_config))
            .collect::<std::result::Result<Vec<_>, _>>()?
    };

    let match_spec_options = ParseMatchSpecOptions::strict();
    let mut specs: Vec<MatchSpec> = Vec::new();
    for dep in &deps.dependencies {
        specs.push(MatchSpec::from_str(dep, match_spec_options)?);
    }

    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;

    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    let gateway = Gateway::builder()
        .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(PackageCache::new(
            rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
        ))
        .with_client(download_client.clone())
        .finish();

    let install_platform = Platform::current();
    let platforms = vec![install_platform, Platform::NoArch];

    const MAX_RETRIES: u32 = 3;
    const INITIAL_DELAY_MS: u64 = 1000;

    let mut last_error = None;
    let mut repo_data = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = INITIAL_DELAY_MS * (1 << (attempt - 1));
            info!(
                "Retrying repodata fetch for sync (attempt {}/{}) after {}ms...",
                attempt + 1,
                MAX_RETRIES,
                delay_ms
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        match gateway
            .query(channels.clone(), platforms.clone(), specs.clone())
            .recursive(true)
            .await
        {
            Ok(data) => {
                repo_data = Some(data);
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                let is_retryable = error_str.contains("500")
                    || error_str.contains("502")
                    || error_str.contains("503")
                    || error_str.contains("504")
                    || error_str.contains("timeout")
                    || error_str.contains("connection");

                if is_retryable && attempt < MAX_RETRIES - 1 {
                    last_error = Some(e);
                    continue;
                }
                return Err(anyhow!("Failed to fetch package metadata: {}", e));
            }
        }
    }

    let repo_data = repo_data.ok_or_else(|| {
        anyhow!(
            "Failed to fetch package metadata after {} retries: {}",
            MAX_RETRIES,
            last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        )
    })?;

    let virtual_packages = rattler_virtual_packages::VirtualPackage::detect(
        &rattler_virtual_packages::VirtualPackageOverrides::default(),
    )?
    .iter()
    .map(|vpkg| GenericVirtualPackage::from(vpkg.clone()))
    .collect::<Vec<_>>();

    let installed_packages = PrefixRecord::collect_from_prefix::<PrefixRecord>(&env.env_path)?;

    let solver_task = SolverTask {
        virtual_packages,
        specs,
        locked_packages: installed_packages
            .iter()
            .map(|r| r.repodata_record.clone())
            .collect(),
        ..SolverTask::from_iter(&repo_data)
    };

    let solver_result = resolvo::Solver.solve(solver_task)?;
    let required_packages = solver_result.records;

    info!("Installing {} packages for sync", required_packages.len());

    Installer::new()
        .with_download_client(download_client)
        .with_target_platform(install_platform)
        .with_installed_packages(installed_packages)
        .install(&env.env_path, required_packages)
        .await?;

    info!("Conda dependencies synced successfully");
    Ok(())
}

/// No-op cleanup (cached environments are kept for reuse).
pub async fn cleanup_environment(_env: &CondaEnvironment) -> Result<()> {
    Ok(())
}

/// Force remove a cached environment.
#[allow(dead_code)]
pub async fn remove_environment(env: &CondaEnvironment) -> Result<()> {
    if env.env_path.exists() {
        tokio::fs::remove_dir_all(&env.env_path).await?;
    }
    Ok(())
}

/// Clear all cached conda environments.
#[allow(dead_code)]
pub async fn clear_cache() -> Result<()> {
    let cache_dir = default_cache_dir_conda();
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
