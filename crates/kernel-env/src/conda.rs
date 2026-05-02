//! Conda environment management via rattler.
//!
//! Creates, caches, and prewarms conda environments for Jupyter kernels.
//! Environments are keyed by a SHA-256 hash of (dependencies + channels +
//! python constraint + env_id) and stored under the cache directory.

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use rattler::{default_cache_dir, install::Installer};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions,
    ParseStrictness, Platform, PrefixRecord, Version, VersionSpec,
};
use rattler_solve::{resolvo, SolverImpl, SolverTask};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::str::FromStr;
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
///
/// Channel-aware; see [`super::uv::default_cache_dir_uv`] for the
/// nightly/stable/dev namespacing rationale.
pub fn default_cache_dir_conda() -> PathBuf {
    runt_workspace::daemon_base_dir().join("conda-envs")
}

/// Base package set every Conda kernel env is warmed with.
///
/// Used by the daemon's Conda pool warmer (`conda_prewarmed_packages` in
/// runtimed) and by the unified env design's capture step (`strip_base`) so
/// the notebook's metadata records only user-level deps. Keep this in sync
/// with the warmer.
pub const CONDA_BASE_PACKAGES: &[&str] = &[
    "ipykernel",
    "ipywidgets",
    "anywidget",
    "nbformat",
    "pyarrow>=14",
];

const CONDA_GIL_SELECTOR: &str = "python-gil";

fn conda_python_requests_free_threading(python: &str) -> bool {
    python
        .split(|ch: char| {
            ch == ','
                || ch == '='
                || ch == '<'
                || ch == '>'
                || ch == '!'
                || ch == '~'
                || ch.is_whitespace()
        })
        .filter(|part| !part.is_empty())
        .any(|part| part.ends_with('t'))
}

fn should_enforce_gil_python(deps: &CondaDependencies) -> bool {
    !deps
        .python
        .as_deref()
        .is_some_and(conda_python_requests_free_threading)
}

/// Compute the unified env hash for a notebook. Used by the captured-deps
/// reopen path from the unified env resolution design (see
/// `docs/superpowers/specs/2026-04-20-unified-env-resolution.md`).
///
/// Requires `deps.env_id` to be `Some`. Distinct from [`compute_env_hash`]
/// only in that contract: the existing function tolerates `env_id = None`
/// for cross-notebook sharing, and this one doesn't. Hash output is
/// identical when `env_id` is `Some` — no on-disk migration needed when
/// PR 2 switches callers.
pub fn compute_unified_env_hash(deps: &CondaDependencies, env_id: &str) -> String {
    let mut with_id = deps.clone();
    with_id.env_id = Some(env_id.to_string());
    compute_env_hash(&with_id)
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

    if should_enforce_gil_python(deps) {
        hasher.update(b"python-abi:gil\n");
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

/// Prepare a Conda environment using the unified env hash
/// (`hash(user_deps, env_id)`).
///
/// This is the reopen path from the unified env resolution design: notebooks
/// with captured deps in their metadata route through here and skip the pool
/// entirely. Behavior mirrors [`prepare_environment_in`] (cache hit →
/// lock-based rebuild → full solve), but the cache key is
/// [`compute_unified_env_hash`]. `env_id` is always required so each
/// notebook's env is isolated on disk from other notebooks with the same dep
/// set.
pub async fn prepare_environment_unified(
    deps: &CondaDependencies,
    env_id: &str,
    cache_dir: &Path,
    handler: Arc<dyn ProgressHandler>,
) -> Result<CondaEnvironment> {
    let hash = compute_unified_env_hash(deps, env_id);
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
        info!("Using cached unified conda env at {:?}", env_path);
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

    info!("Creating new unified conda env at {:?}", env_path);

    tokio::fs::create_dir_all(cache_dir).await?;

    // Try lock-based rebuild before full re-creation
    if env_path.exists() && !python_path.exists() {
        if let Some(lock) = crate::lock::LockFile::read_from(&env_path).await {
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
            &format_python_spec(py),
            match_spec_options,
        )?);
    } else {
        specs.push(MatchSpec::from_str("python>=3.13", match_spec_options)?);
    }
    if should_enforce_gil_python(deps) {
        specs.push(MatchSpec::from_str(CONDA_GIL_SELECTOR, match_spec_options)?);
    }

    specs.push(MatchSpec::from_str("ipykernel", match_spec_options)?);
    specs.push(MatchSpec::from_str("ipywidgets", match_spec_options)?);
    specs.push(MatchSpec::from_str("anywidget", match_spec_options)?);
    specs.push(MatchSpec::from_str("nbformat", match_spec_options)?);
    specs.push(MatchSpec::from_str("pyarrow>=14", match_spec_options)?);

    for dep in &deps.dependencies {
        if dep != "ipykernel"
            && dep != "ipywidgets"
            && dep != "anywidget"
            && dep != "nbformat"
            && dep != "pyarrow>=14"
        {
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
/// on `(user_defaults, env_id)`, so it will be found by
/// [`prepare_environment_unified`] later. `user_defaults` is the pool
/// env's full install list with [`CONDA_BASE_PACKAGES`] stripped — the
/// user-level deps that belong in the notebook's metadata.
pub async fn claim_prewarmed_environment(
    prewarmed: CondaEnvironment,
    env_id: &str,
    user_defaults: &[String],
) -> Result<CondaEnvironment> {
    claim_prewarmed_environment_in(prewarmed, env_id, user_defaults, &default_cache_dir_conda())
        .await
}

/// Like [`claim_prewarmed_environment`] but with an explicit cache directory.
pub async fn claim_prewarmed_environment_in(
    prewarmed: CondaEnvironment,
    env_id: &str,
    user_defaults: &[String],
    cache_dir: &Path,
) -> Result<CondaEnvironment> {
    let deps = CondaDependencies {
        dependencies: user_defaults.to_vec(),
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: None,
    };
    let hash = compute_unified_env_hash(&deps, env_id);
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
/// already-installed packages as locked. Progress events (solve, download,
/// link, install-complete, ready) flow through `handler` so the frontend
/// banner and logs stay in sync with the real transaction — including the
/// full package count when the solver pulls in transitive deps.
pub async fn sync_dependencies(
    env: &CondaEnvironment,
    deps: &CondaDependencies,
    handler: Arc<dyn ProgressHandler>,
) -> Result<()> {
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

    // Pin the installed Python version so the solver cannot upgrade or
    // downgrade it. Without this, the solver treats Python as a soft
    // preference (locked_packages) and can swap it out to satisfy new
    // deps — producing site-packages for the wrong Python version.
    // Also enforce `python-gil` to prevent switching to the
    // free-threaded build. See: conda-sequential pinning bug.
    let installed_python_version = detect_installed_python_version(&env.env_path);

    // Always include base runtime packages — the solver only returns packages
    // needed to satisfy specs, and locked_packages are "preferred" not "required".
    // Without these, the Installer will remove ipykernel etc from the env.
    let mut specs: Vec<MatchSpec> = vec![
        MatchSpec::from_str("ipykernel", match_spec_options)?,
        MatchSpec::from_str("ipywidgets", match_spec_options)?,
        MatchSpec::from_str("anywidget", match_spec_options)?,
        MatchSpec::from_str("nbformat", match_spec_options)?,
        MatchSpec::from_str("pyarrow>=14", match_spec_options)?,
    ];

    if let Some(ref py_ver) = installed_python_version {
        info!("Pinning Python to installed version: {}", py_ver);
        specs.push(MatchSpec::from_str(
            &format!("python={}", py_ver),
            match_spec_options,
        )?);
        // Preserve the installed GIL/free-threaded selector. conda-meta
        // stores "3.14.4" not "3.14t", so we can't infer from the version
        // string. Instead, check if `python-freethreading` is installed:
        // if so, keep `python-freethreading`; otherwise pin `python-gil`.
        if has_freethreading_package(&env.env_path) {
            info!("Free-threaded Python detected, keeping python-freethreading selector");
            specs.push(MatchSpec::from_str(
                "python-freethreading",
                match_spec_options,
            )?);
        } else {
            specs.push(MatchSpec::from_str(CONDA_GIL_SELECTOR, match_spec_options)?);
        }
    } else {
        warn!(
            "Could not detect installed Python version in {:?}, solver may change Python",
            env.env_path
        );
    }

    for dep in &deps.dependencies {
        if dep != "ipykernel"
            && dep != "ipywidgets"
            && dep != "anywidget"
            && dep != "nbformat"
            && dep != "pyarrow>=14"
        {
            specs.push(MatchSpec::from_str(dep, match_spec_options)?);
        }
    }

    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;

    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

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
    // The solver pulls in transitive deps; emit the real count here so the
    // banner doesn't stay pinned at the (usually smaller) user-dep count.
    handler.on_progress(
        "conda",
        EnvProgressPhase::Installing {
            total: required_packages.len(),
        },
    );

    let reporter = RattlerReporter::new(handler.clone());
    let install_start = Instant::now();

    Installer::new()
        .with_download_client(download_client)
        .with_target_platform(install_platform)
        .with_installed_packages(installed_packages)
        .with_reporter(reporter)
        .install(&env.env_path, required_packages)
        .await?;

    handler.on_progress(
        "conda",
        EnvProgressPhase::InstallComplete {
            elapsed_ms: install_start.elapsed().as_millis() as u64,
        },
    );

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

/// Format a Python version constraint as a conda MatchSpec string.
///
/// Bare versions (`"3.11"`, `"3.11.*"`) get `python=` prepended.
/// Operator-prefixed constraints (`">=3.9"`, `">=3.9,<4"`) get `python`
/// prepended without an extra `=`, producing e.g. `"python>=3.9,<4"`.
fn format_python_spec(constraint: &str) -> String {
    let first = constraint.as_bytes().first().copied().unwrap_or(b'0');
    if first == b'>' || first == b'<' || first == b'=' || first == b'!' || first == b'~' {
        format!("python{}", constraint)
    } else {
        format!("python={}", constraint)
    }
}

/// Build the list of spec strings that `install_conda_env` would produce,
/// for matching against a lock file.
fn build_spec_strings(deps: &CondaDependencies) -> Vec<String> {
    let mut specs = Vec::new();

    if let Some(ref py) = deps.python {
        specs.push(format_python_spec(py));
    } else {
        specs.push("python>=3.13".to_string());
    }
    if should_enforce_gil_python(deps) {
        specs.push(CONDA_GIL_SELECTOR.to_string());
    }

    specs.push("ipykernel".to_string());
    specs.push("ipywidgets".to_string());
    specs.push("anywidget".to_string());
    specs.push("nbformat".to_string());
    specs.push("pyarrow>=14".to_string());

    for dep in &deps.dependencies {
        if dep != "ipykernel"
            && dep != "ipywidgets"
            && dep != "anywidget"
            && dep != "nbformat"
            && dep != "pyarrow>=14"
        {
            specs.push(dep.clone());
        }
    }

    specs
}

/// Detect the Python version installed in a conda environment by reading
/// `conda-meta/python-*.json`. Returns `"major.minor.patch"` (e.g. `"3.14.4"`).
///
/// This is cheaper than spawning `python --version` and works even when the
/// environment's Python is broken or missing from PATH.
pub fn detect_installed_python_version(env_path: &std::path::Path) -> Option<String> {
    let meta_dir = env_path.join("conda-meta");
    let entries = std::fs::read_dir(&meta_dir).ok()?;
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        // conda-meta filenames: python-3.14.4-h0abcdef_0.json
        if let Some(rest) = fname.strip_prefix("python-") {
            if let Some(stem) = rest.strip_suffix(".json") {
                // Extract version: everything before the first '-' after the version
                // e.g. "3.14.4-h0abcdef_0" → "3.14.4"
                let version = stem.split('-').next().unwrap_or(stem);
                if version.contains('.') {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

/// Check whether the `python-freethreading` package is installed in a conda
/// environment by looking for `python-freethreading-*.json` in `conda-meta`.
///
/// `conda-meta` stores the plain version number (e.g. `3.14.4`), not the
/// `3.14t` constraint syntax, so we cannot infer free-threading from the
/// Python version string. Instead, check for the selector package that conda
/// uses to distinguish GIL vs free-threaded builds. If `python-freethreading`
/// is present, the env was created as free-threaded; otherwise it uses the
/// default GIL build.
fn has_freethreading_package(env_path: &std::path::Path) -> bool {
    let meta_dir = env_path.join("conda-meta");
    let entries = match std::fs::read_dir(&meta_dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        // e.g. python-freethreading-3.14.4-h0abcdef_0.json
        if fname.starts_with("python-freethreading-") && fname.ends_with(".json") {
            return true;
        }
    }
    false
}

/// Check whether the installed Python version in a conda env matches the
/// requested constraint from environment.yaml (e.g. `"3.12.*"`, `">=3.9,<4"`).
///
/// Delegates to rattler's [`VersionSpec::matches`] for full conda version
/// constraint semantics — exact pins, ranges, wildcards, compound AND/OR
/// specs, compatible-release (`~=`), and glob patterns all work correctly.
///
/// Returns `true` if the constraint is satisfied or if either version can't
/// be determined (fail-open to avoid unnecessary rebuilds). Returns `false`
/// only when the installed version clearly violates the constraint.
///
/// Used by the `conda:env_yml` launch path to detect when an existing named
/// env has a different Python version than what environment.yaml requests,
/// triggering a rebuild instead of syncing into the wrong Python.
pub fn installed_python_matches_constraint(env_path: &std::path::Path, requested: &str) -> bool {
    let installed_str = match detect_installed_python_version(env_path) {
        Some(v) => v,
        None => return true, // Can't detect → fail-open
    };

    let installed = match Version::from_str(&installed_str) {
        Ok(v) => v,
        Err(_) => return true, // Unusual format → fail-open
    };

    // Strip the free-threaded Python `t` selector (e.g. "3.14t" → "3.14").
    // Conda records the installed version as plain "3.14.4" in conda-meta
    // and tracks free-threading via the separate `python-freethreading`
    // package. The `t` suffix is a build selector, not a version component,
    // so we remove it before version matching.
    let stripped = strip_free_threading_selector(requested);
    let constraint = stripped.as_deref().unwrap_or(requested);

    // Bare versions like "3.12" should match any 3.12.x (conda pin
    // semantics). Rattler treats a bare version as ==3.12.0 which is too
    // strict. Append ".*" to each bare-version term so rattler uses
    // starts-with matching. Terms with operators (>=, <, ==) or existing
    // wildcards pass through unchanged. Handles compound specs like
    // "3.10|3.11" by normalizing each OR/AND branch independently.
    let normalized = normalize_bare_versions(constraint);
    let spec_str = normalized.as_deref().unwrap_or(constraint);

    let spec = match VersionSpec::from_str(spec_str, ParseStrictness::Lenient) {
        Ok(s) => s,
        Err(_) => return true, // Unparseable constraint → fail-open
    };

    spec.matches(&installed)
}

/// Strip the free-threaded Python `t` selector from a version constraint.
///
/// Returns `Some(cleaned)` if any `t` suffix was removed, `None` if the
/// string was already clean. Handles both bare versions (`"3.14t"`),
/// wildcard versions (`"3.14t.*"`), and operator-prefixed constraints
/// (`">=3.14t"`).
fn strip_free_threading_selector(constraint: &str) -> Option<String> {
    // Quick check — if there's no `t` at all, nothing to strip.
    if !constraint.contains('t') {
        return None;
    }
    let mut changed = false;
    let mut result = String::with_capacity(constraint.len());
    for (i, clause) in constraint.split(',').enumerate() {
        if i > 0 {
            result.push(',');
        }
        let trimmed = clause.trim();
        // Find where the version digits start (after >=, <=, ==, >, <, =, ~=)
        let version_start = trimmed
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(trimmed.len());
        let (prefix, version_part) = trimmed.split_at(version_start);
        // Strip trailing `.*` glob, check for `t`, then reattach glob.
        let (core, glob_suffix) = version_part
            .strip_suffix(".*")
            .map_or((version_part, ""), |c| (c, ".*"));
        if core.ends_with('t') && core.len() > 1 && core.as_bytes()[core.len() - 2] != b'.' {
            changed = true;
            result.push_str(prefix);
            result.push_str(&core[..core.len() - 1]);
            result.push_str(glob_suffix);
        } else {
            result.push_str(trimmed);
        }
    }
    if changed {
        Some(result)
    } else {
        None
    }
}

/// Normalize bare version terms in a constraint string to wildcard pins.
///
/// Splits on `|` (OR) and `,` (AND) boundaries, and appends `.*` to any
/// term that starts with a digit and doesn't already contain a wildcard or
/// operator. Returns `None` if no normalization was needed.
///
/// Examples:
/// - `"3.12"` → `Some("3.12.*")`
/// - `"3.10|3.11"` → `Some("3.10.*|3.11.*")`
/// - `">=3.9,<4"` → `None` (no bare versions)
fn normalize_bare_versions(constraint: &str) -> Option<String> {
    if constraint.is_empty() {
        return None;
    }
    let mut changed = false;
    let mut result = String::with_capacity(constraint.len() + 4);
    // Split on `|` first (OR), then each branch on `,` (AND).
    for (i, or_branch) in constraint.split('|').enumerate() {
        if i > 0 {
            result.push('|');
        }
        for (j, term) in or_branch.split(',').enumerate() {
            if j > 0 {
                result.push(',');
            }
            let trimmed = term.trim();
            if !trimmed.is_empty()
                && trimmed.as_bytes()[0].is_ascii_digit()
                && !trimmed.contains('*')
            {
                changed = true;
                result.push_str(trimmed);
                result.push_str(".*");
            } else {
                result.push_str(trimmed);
            }
        }
    }
    if changed {
        Some(result)
    } else {
        None
    }
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

    // ── unified env hash (PR 1, spec 2026-04-20) ─────────────────────────

    #[test]
    fn unified_hash_matches_legacy_with_env_id() {
        // Conda's legacy hash already always includes env_id, so the unified
        // hash produces identical output for the same inputs. Sanity-check
        // the bridge so switching callers in PR 2 doesn't invalidate any
        // on-disk env.
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into(), "scipy".into()],
            channels: vec!["conda-forge".into()],
            python: None,
            env_id: Some("abc".into()),
        };
        assert_eq!(
            compute_env_hash(&deps),
            compute_unified_env_hash(&deps, "abc"),
        );
    }

    #[test]
    fn unified_hash_overrides_env_id_in_deps() {
        // If the caller already populated deps.env_id, the unified function
        // uses the explicit env_id argument. This lets us pass in-flight
        // values without reshaping the struct.
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: None,
            env_id: Some("stale".into()),
        };
        let with_explicit = compute_unified_env_hash(&deps, "fresh");
        let mut expected_deps = deps.clone();
        expected_deps.env_id = Some("fresh".into());
        assert_eq!(with_explicit, compute_env_hash(&expected_deps));
    }

    #[test]
    fn unified_hash_isolates_by_env_id() {
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: None,
            env_id: None,
        };
        let h1 = compute_unified_env_hash(&deps, "notebook-1");
        let h2 = compute_unified_env_hash(&deps, "notebook-2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn managed_specs_enforce_gil_python_by_default() {
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: None,
            env_id: None,
        };

        let specs = build_spec_strings(&deps);
        assert!(specs.contains(&"python>=3.13".to_string()));
        assert!(specs.contains(&CONDA_GIL_SELECTOR.to_string()));
    }

    #[test]
    fn managed_specs_enforce_gil_python_for_normal_pin() {
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: Some("3.11".into()),
            env_id: None,
        };

        let specs = build_spec_strings(&deps);
        assert!(specs.contains(&"python=3.11".to_string()));
        assert!(specs.contains(&CONDA_GIL_SELECTOR.to_string()));
    }

    #[test]
    fn explicit_free_threaded_pin_does_not_add_gil_selector() {
        let deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: Some("3.14t".into()),
            env_id: None,
        };

        let specs = build_spec_strings(&deps);
        assert!(specs.contains(&"python=3.14t".to_string()));
        assert!(!specs.contains(&CONDA_GIL_SELECTOR.to_string()));
    }

    #[test]
    fn free_threading_constraint_detection_is_explicit() {
        assert!(conda_python_requests_free_threading("3.14t"));
        assert!(conda_python_requests_free_threading(">=3.14t"));
        assert!(conda_python_requests_free_threading(">=3.13,!=3.14t"));
        assert!(!conda_python_requests_free_threading("3.14"));
        assert!(!conda_python_requests_free_threading(">=3.13,<3.15"));
    }

    #[test]
    fn gil_policy_changes_cache_identity() {
        let gil_deps = CondaDependencies {
            dependencies: vec!["numpy".into()],
            channels: vec!["conda-forge".into()],
            python: Some("3.14".into()),
            env_id: None,
        };
        let free_threaded_deps = CondaDependencies {
            python: Some("3.14t".into()),
            ..gil_deps.clone()
        };

        assert_ne!(
            compute_env_hash(&gil_deps),
            compute_env_hash(&free_threaded_deps)
        );
    }

    #[test]
    fn detect_installed_python_version_reads_conda_meta() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.4-h2b28147_0.json"), "{}").unwrap();

        let version = detect_installed_python_version(dir.path());
        assert_eq!(version.as_deref(), Some("3.14.4"));
    }

    #[test]
    fn detect_installed_python_version_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        // Only numpy, no python
        std::fs::write(meta.join("numpy-2.4.3-py314h2b28147_0.json"), "{}").unwrap();

        let version = detect_installed_python_version(dir.path());
        assert_eq!(version, None);
    }

    #[test]
    fn detect_installed_python_version_free_threaded() {
        // Free-threaded builds have version like 3.14t in the spec
        // but conda-meta uses the real version number (3.14.4)
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.4-h2b28147_0_cpython.json"), "{}").unwrap();

        let version = detect_installed_python_version(dir.path());
        assert_eq!(version.as_deref(), Some("3.14.4"));
    }

    #[test]
    fn has_freethreading_package_detects_selector() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.4-h2b28147_0_cpython.json"), "{}").unwrap();
        std::fs::write(
            meta.join("python-freethreading-3.14.4-h2b28147_0.json"),
            "{}",
        )
        .unwrap();

        assert!(has_freethreading_package(dir.path()));
    }

    #[test]
    fn has_freethreading_package_false_for_gil_env() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.4-h2b28147_0.json"), "{}").unwrap();

        assert!(!has_freethreading_package(dir.path()));
    }

    #[test]
    fn has_freethreading_package_false_when_no_conda_meta() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_freethreading_package(dir.path()));
    }

    #[test]
    fn python_constraint_matches_same_major_minor() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.12.7-h2b28147_0.json"), "{}").unwrap();

        // Bare "3.12" → normalized to "3.12.*" (conda pin semantics)
        assert!(installed_python_matches_constraint(dir.path(), "3.12"));
        // Exact patch version
        assert!(installed_python_matches_constraint(dir.path(), "3.12.7"));
        // Range constraint
        assert!(installed_python_matches_constraint(dir.path(), ">=3.12"));
        // Wildcard pin (as rattler emits from environment.yml parsing)
        assert!(installed_python_matches_constraint(dir.path(), "3.12.*"));
        // ==3.12 means exactly 3.12.0 in conda — 3.12.7 does NOT match
        assert!(!installed_python_matches_constraint(dir.path(), "==3.12"));
        // ==3.12.7 is an exact match
        assert!(installed_python_matches_constraint(dir.path(), "==3.12.7"));
    }

    #[test]
    fn python_constraint_rejects_different_major_minor() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.4-h2b28147_0.json"), "{}").unwrap();

        assert!(!installed_python_matches_constraint(dir.path(), "3.12"));
        assert!(!installed_python_matches_constraint(dir.path(), "3.13"));
    }

    #[test]
    fn python_constraint_range_ge_accepts_higher() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.12.4-h2b28147_0.json"), "{}").unwrap();

        // 3.12 >= 3.9 → true
        assert!(installed_python_matches_constraint(dir.path(), ">=3.9"));
        // 3.12 >= 3.12 → true
        assert!(installed_python_matches_constraint(dir.path(), ">=3.12"));
        // 3.12 >= 3.13 → false
        assert!(!installed_python_matches_constraint(dir.path(), ">=3.13"));
    }

    #[test]
    fn python_constraint_range_lt_rejects_higher() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.12.4-h2b28147_0.json"), "{}").unwrap();

        // 3.12 < 3.13 → true
        assert!(installed_python_matches_constraint(dir.path(), "<3.13"));
        // 3.12 < 3.12 → false
        assert!(!installed_python_matches_constraint(dir.path(), "<3.12"));
    }

    #[test]
    fn python_constraint_major_only_matches() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.12.4-h2b28147_0.json"), "{}").unwrap();

        // Major-only: "3" or "3.*" → any 3.x is fine
        assert!(installed_python_matches_constraint(dir.path(), "3"));
        assert!(installed_python_matches_constraint(dir.path(), "3.*"));
        assert!(!installed_python_matches_constraint(dir.path(), "2"));
    }

    #[test]
    fn python_constraint_comma_range_both_clauses() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.11.0-h2b28147_0.json"), "{}").unwrap();

        // ">=3.9,<3.13" → 3.11 >= 3.9 AND 3.11 < 3.13 → true
        assert!(installed_python_matches_constraint(
            dir.path(),
            ">=3.9,<3.13"
        ));
    }

    #[test]
    fn python_constraint_upper_bound_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.14.0-h2b28147_0.json"), "{}").unwrap();

        // ">=3.9,<3.13" → 3.14 >= 3.9 but 3.14 NOT < 3.13 → false
        assert!(!installed_python_matches_constraint(
            dir.path(),
            ">=3.9,<3.13"
        ));
    }

    #[test]
    fn python_constraint_major_only_lt() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.11.0-h2b28147_0.json"), "{}").unwrap();

        // ">=3.9,<4" → 3.11 >= 3.9 AND 3 < 4 → true
        assert!(installed_python_matches_constraint(dir.path(), ">=3.9,<4"));
    }

    #[test]
    fn python_constraint_fails_open_when_no_env() {
        let dir = tempfile::tempdir().unwrap();
        // No conda-meta → can't detect → returns true (fail-open)
        assert!(installed_python_matches_constraint(dir.path(), "3.12"));
    }

    #[test]
    fn python_constraint_free_threaded_selector_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        // conda-meta records "3.14.4" (no `t` suffix)
        std::fs::write(meta.join("python-3.14.4-h2b28147_0.json"), "{}").unwrap();

        // Free-threaded pins: the `t` selector is stripped before matching
        assert!(installed_python_matches_constraint(dir.path(), "3.14t"));
        assert!(installed_python_matches_constraint(dir.path(), "3.14t.*"));
        assert!(installed_python_matches_constraint(dir.path(), ">=3.14t"));
        assert!(!installed_python_matches_constraint(dir.path(), ">=3.15t"));
    }

    #[test]
    fn strip_free_threading_selector_cases() {
        assert_eq!(strip_free_threading_selector("3.14t"), Some("3.14".into()));
        assert_eq!(
            strip_free_threading_selector("3.14t.*"),
            Some("3.14.*".into())
        );
        assert_eq!(
            strip_free_threading_selector(">=3.14t"),
            Some(">=3.14".into())
        );
        assert_eq!(
            strip_free_threading_selector(">=3.14t,<4"),
            Some(">=3.14,<4".into())
        );
        assert_eq!(strip_free_threading_selector("3.14"), None);
        assert_eq!(strip_free_threading_selector(">=3.9,<4"), None);
    }

    #[test]
    fn python_constraint_or_branches() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("python-3.11.0-h2b28147_0.json"), "{}").unwrap();

        // OR constraint: "3.10|3.11" → 3.11 matches second branch
        assert!(installed_python_matches_constraint(dir.path(), "3.10|3.11"));
        // Neither branch matches
        assert!(!installed_python_matches_constraint(
            dir.path(),
            "3.12|3.13"
        ));
    }

    #[test]
    fn normalize_bare_versions_cases() {
        assert_eq!(normalize_bare_versions("3.12"), Some("3.12.*".into()));
        assert_eq!(
            normalize_bare_versions("3.10|3.11"),
            Some("3.10.*|3.11.*".into())
        );
        assert_eq!(normalize_bare_versions(">=3.9,<4"), None);
        assert_eq!(normalize_bare_versions("3.12.*"), None);
        assert_eq!(
            normalize_bare_versions("3.10|>=3.11"),
            Some("3.10.*|>=3.11".into())
        );
    }

    #[test]
    fn format_python_spec_bare_version() {
        assert_eq!(format_python_spec("3.11"), "python=3.11");
        assert_eq!(format_python_spec("3.11.*"), "python=3.11.*");
    }

    #[test]
    fn format_python_spec_operator_prefixed() {
        assert_eq!(format_python_spec(">=3.9"), "python>=3.9");
        assert_eq!(format_python_spec(">=3.9,<4"), "python>=3.9,<4");
        assert_eq!(format_python_spec("==3.12"), "python==3.12");
        assert_eq!(format_python_spec("<4"), "python<4");
    }
}
