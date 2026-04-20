//! Conda environment management via rattler.
//!
//! Creates, caches, and prewarms conda environments for Jupyter kernels.
//! Environments are keyed by a SHA-256 hash of (dependencies + channels +
//! python constraint + env_id) and stored under the cache directory.

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use rattler::{default_cache_dir, install::Installer};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions, Platform,
    PrefixRecord,
};
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
    hex::encode(hash)[..16].to_string()
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
        crate::launcher::vendor_into_venv(&python_path)
            .await
            .context("vendor nteract_kernel_launcher into conda env")?;
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

    // Try lock-based rebuild before full re-creation
    if env_path.exists() && !python_path.exists() {
        if let Some(lock) = crate::lock::LockFile::read_from(&env_path).await {
            // Build expected specs to match against the lock
            let expected_specs = build_spec_strings(deps);
            let expected_channels = if deps.channels.is_empty() {
                vec!["conda-forge".to_string()]
            } else {
                deps.channels.clone()
            };
            if lock.matches(&expected_specs, &expected_channels) {
                info!("Rebuilding conda env from lock file at {:?}", env_path);
                tokio::fs::remove_dir_all(&env_path).await?;
                tokio::fs::create_dir_all(&env_path).await?;
                match crate::lock::install_from_lock(&env_path, &lock, handler.clone(), "conda")
                    .await
                {
                    Ok(()) => {
                        if python_path.exists() {
                            // Re-persist lock so it survives future rebuilds
                            crate::lock::try_write_lock(&env_path, &lock).await;
                            crate::gc::touch_last_used(&env_path).await;
                            crate::launcher::vendor_into_venv(&python_path)
                                .await
                                .context("vendor nteract_kernel_launcher into conda env")?;
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
                    }
                    Err(e) => {
                        warn!(
                            "Lock-based rebuild failed: {}, falling back to full solve",
                            e
                        );
                        tokio::fs::remove_dir_all(&env_path).await.ok();
                    }
                }
            }
        }
    }

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
    crate::launcher::vendor_into_venv(&python_path)
        .await
        .context("vendor nteract_kernel_launcher into conda env")?;
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
            channels: channel_names.clone(),
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

    // Capture spec strings for lock file using the same format as build_spec_strings()
    // (raw input strings, not MatchSpec::to_string() which may normalize differently)
    let spec_strings_for_lock = build_spec_strings(deps);

    // Rattler cache
    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;
    rattler_cache::ensure_cache_dir(&rattler_cache_dir)
        .map_err(|e| anyhow!("could not create rattler cache directory: {}", e))?;

    // HTTP client
    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    // Query repodata with offline-first strategy
    let install_platform = Platform::current();
    let platforms = vec![install_platform, Platform::NoArch];

    let repo_data = crate::repodata::query_repodata_offline_first(
        channels,
        platforms,
        specs.clone(),
        &rattler_cache_dir,
        download_client.clone(),
        handler.clone(),
        "conda",
    )
    .await?;

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

    // Clone packages before install (which consumes them) so we can write the lock file
    let packages_for_lock = required_packages.clone();

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

    // Write lock file for offline re-creation
    let lock = crate::lock::LockFile::new(spec_strings_for_lock, channel_names, packages_for_lock);
    crate::lock::try_write_lock(env_path, &lock).await;

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

    crate::launcher::vendor_into_venv(&env.python_path)
        .await
        .context("vendor nteract_kernel_launcher into prewarmed conda env")?;

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
        crate::launcher::vendor_into_venv(&python_path)
            .await
            .context("vendor nteract_kernel_launcher into claimed conda env")?;
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

    crate::launcher::vendor_into_venv(&python_path)
        .await
        .context("vendor nteract_kernel_launcher into claimed conda env")?;

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

    let site_packages = find_site_packages(&env.env_path);
    let warmup_script = crate::warmup::build_warmup_command(&[], true, site_packages.as_deref());

    let output = tokio::process::Command::new(&env.python_path)
        .args(["-c", &warmup_script])
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

    // Always include base runtime packages — the solver only returns packages
    // needed to satisfy specs, and locked_packages are "preferred" not "required".
    // Without these, the Installer will remove ipykernel etc from the env.
    let mut specs: Vec<MatchSpec> = vec![
        MatchSpec::from_str("ipykernel", match_spec_options)?,
        MatchSpec::from_str("ipywidgets", match_spec_options)?,
        MatchSpec::from_str("anywidget", match_spec_options)?,
        MatchSpec::from_str("nbformat", match_spec_options)?,
    ];

    for dep in &deps.dependencies {
        if dep != "ipykernel" && dep != "ipywidgets" && dep != "anywidget" && dep != "nbformat" {
            specs.push(MatchSpec::from_str(dep, match_spec_options)?);
        }
    }

    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;

    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    let install_platform = Platform::current();
    let platforms = vec![install_platform, Platform::NoArch];

    // Use LogHandler for sync operations (no UI progress needed)
    let handler = Arc::new(crate::progress::LogHandler);

    let repo_data = crate::repodata::query_repodata_offline_first(
        channels,
        platforms,
        specs.clone(),
        &rattler_cache_dir,
        download_client.clone(),
        handler,
        "conda",
    )
    .await?;

    let virtual_packages = rattler_virtual_packages::VirtualPackage::detect(
        &rattler_virtual_packages::VirtualPackageOverrides::default(),
    )?
    .iter()
    .map(|vpkg| GenericVirtualPackage::from(vpkg.clone()))
    .collect::<Vec<_>>();

    let installed_packages = PrefixRecord::collect_from_prefix::<PrefixRecord>(&env.env_path)?;

    // Collect package names that have explicit version constraints in specs.
    // Exclude these from locked_packages so the solver doesn't favor installed
    // versions that violate the requested constraints (e.g., scikit-learn==1.8.0
    // locked when spec says <1.6).
    let constrained_names: std::collections::HashSet<String> = specs
        .iter()
        .filter(|s| s.version.is_some())
        .filter_map(|s| match &s.name {
            rattler_conda_types::PackageNameMatcher::Exact(name) => {
                Some(name.as_normalized().to_string())
            }
            _ => None,
        })
        .collect();

    let solver_task = SolverTask {
        virtual_packages,
        specs,
        locked_packages: installed_packages
            .iter()
            .filter(|r| {
                let name = r.repodata_record.package_record.name.as_normalized();
                !constrained_names.contains(name)
            })
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

/// Build the list of spec strings that `install_conda_env` would produce,
/// for matching against a lock file.
fn build_spec_strings(deps: &CondaDependencies) -> Vec<String> {
    let mut specs = Vec::new();

    if let Some(ref py) = deps.python {
        specs.push(format!("python={}", py));
    } else {
        specs.push("python>=3.13".to_string());
    }

    specs.push("ipykernel".to_string());
    specs.push("ipywidgets".to_string());
    specs.push("anywidget".to_string());
    specs.push("nbformat".to_string());

    for dep in &deps.dependencies {
        if dep != "ipykernel" && dep != "ipywidgets" && dep != "anywidget" && dep != "nbformat" {
            specs.push(dep.clone());
        }
    }

    specs
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
