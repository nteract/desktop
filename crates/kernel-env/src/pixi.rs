//! Pixi-compatible environment management via rattler.
//!
//! Creates pixi-style project directories with conda environments managed
//! by rattler. This replaces the subprocess-based `pixi init` + `pixi add`
//! approach with direct rattler API calls while maintaining pixi-compatible
//! directory layout (`.pixi/envs/default/`).
//!
//! ## Why not use `pixi_api` directly?
//!
//! `pixi_api` depends on `pixi_core`, which pulls in ~30 pixi subcrates,
//! the entire `uv` resolver stack, and requires rattler 0.40.3 (we use 0.39).
//! The dependency overhead would roughly double compile times. Instead, we
//! use rattler directly (already a dependency) and generate the pixi manifest
//! ourselves — giving us the same result with zero new dependencies.

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use rattler::{default_cache_dir, install::Installer, package_cache::PackageCache};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions, Platform,
};
use rattler_repodata_gateway::Gateway;
use rattler_solve::{resolvo, SolverImpl, SolverTask};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::progress::{EnvProgressPhase, ProgressHandler, RattlerReporter};

/// A resolved pixi environment on disk.
///
/// The directory layout matches what `pixi init` + `pixi add` creates:
/// ```text
/// <project_dir>/
///   pixi.toml
///   .pixi/
///     envs/
///       default/     ← conda prefix (this is venv_path)
///         bin/python ← python_path
/// ```
#[derive(Debug, Clone)]
pub struct PixiEnvironment {
    /// The pixi project directory (contains pixi.toml).
    pub project_dir: PathBuf,
    /// The conda prefix directory (`.pixi/envs/default/`).
    pub venv_path: PathBuf,
    /// Path to the Python binary inside the environment.
    pub python_path: PathBuf,
}

/// Get the default cache directory for pixi project environments.
pub fn default_cache_dir_pixi() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("pixi-envs")
}

/// Generate a minimal `pixi.toml` manifest for the given packages.
///
/// This produces a valid pixi manifest that records the installed packages,
/// channels, and platform. The environment can later be extended by pixi CLI
/// or pixi API if needed.
fn generate_pixi_manifest(name: &str, packages: &[String], channels: &[String]) -> String {
    let platform = Platform::current().to_string();

    let channels_str = channels
        .iter()
        .map(|c| format!("\"{}\"", c))
        .collect::<Vec<_>>()
        .join(", ");

    let deps_str = packages
        .iter()
        .map(|p| {
            // Split "package>=version" into name and version spec
            if let Some(idx) = p.find(|c: char| c == '>' || c == '<' || c == '=' || c == '!') {
                let (name, version) = p.split_at(idx);
                format!("{} = \"{}\"", name.trim(), version.trim())
            } else {
                format!("{} = \"*\"", p)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"[workspace]
channels = [{channels}]
name = "{name}"
platforms = ["{platform}"]
version = "0.1.0"

[tasks]

[dependencies]
{deps}
"#,
        channels = channels_str,
        name = name,
        platform = platform,
        deps = deps_str,
    )
}

/// Create a pixi-compatible environment using rattler.
///
/// This is the core function that replaces `pixi init` + `pixi add` with
/// direct rattler calls. It:
/// 1. Creates the pixi project directory structure
/// 2. Generates a `pixi.toml` manifest
/// 3. Uses rattler to solve and install packages into `.pixi/envs/default/`
///
/// Progress events are emitted via `handler` throughout the lifecycle.
pub async fn create_pixi_environment(
    project_dir: &Path,
    packages: &[String],
    channels: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<PixiEnvironment> {
    let env_path = project_dir.join(".pixi").join("envs").join("default");

    #[cfg(target_os = "windows")]
    let python_path = env_path.join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = env_path.join("bin").join("python");

    handler.on_progress(
        "pixi",
        EnvProgressPhase::Starting {
            env_hash: project_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        },
    );

    // Create project directory structure
    tokio::fs::create_dir_all(&env_path).await?;

    // Generate and write pixi.toml
    let channels = if channels.is_empty() {
        vec!["conda-forge".to_string()]
    } else {
        channels.to_vec()
    };
    let project_name = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "runtimed-pixi".to_string());
    let manifest = generate_pixi_manifest(&project_name, packages, &channels);
    let manifest_path = project_dir.join("pixi.toml");
    tokio::fs::write(&manifest_path, &manifest).await?;
    debug!(
        "[pixi] Wrote pixi.toml to {:?} with {} packages",
        manifest_path,
        packages.len()
    );

    // Install packages using rattler
    install_pixi_env(&env_path, packages, &channels, handler.clone()).await?;

    // Verify python exists
    if !python_path.exists() {
        tokio::fs::remove_dir_all(project_dir).await.ok();
        return Err(anyhow!(
            "Python not found at {:?} after pixi install",
            python_path
        ));
    }

    handler.on_progress(
        "pixi",
        EnvProgressPhase::Ready {
            env_path: env_path.to_string_lossy().to_string(),
            python_path: python_path.to_string_lossy().to_string(),
        },
    );

    Ok(PixiEnvironment {
        project_dir: project_dir.to_path_buf(),
        venv_path: env_path,
        python_path,
    })
}

/// Core rattler solve + install logic for pixi environments.
async fn install_pixi_env(
    env_path: &Path,
    packages: &[String],
    channels: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<()> {
    let cache_dir = env_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("/tmp"))
        .to_path_buf();
    let channel_config = ChannelConfig::default_with_root_dir(cache_dir);

    // Parse channels
    let channels: Vec<Channel> = channels
        .iter()
        .map(|c| Channel::from_str(c, &channel_config))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let channel_names: Vec<String> = channels.iter().map(|c| c.name().to_string()).collect();

    handler.on_progress(
        "pixi",
        EnvProgressPhase::FetchingRepodata {
            channels: channel_names,
        },
    );

    // Build specs — always include python
    let match_spec_options = ParseMatchSpecOptions::strict();
    let mut specs: Vec<MatchSpec> = vec![MatchSpec::from_str("python>=3.13", match_spec_options)?];

    for pkg in packages {
        let spec = MatchSpec::from_str(pkg, match_spec_options)?;
        specs.push(spec);
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
                "[pixi] Retrying repodata fetch (attempt {}/{}) after {}ms...",
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
                        "[pixi] Transient error fetching repodata (attempt {}): {}",
                        attempt + 1,
                        error_str
                    );
                    last_error = Some(e);
                    continue;
                }
                let error_msg = format!("Failed to fetch package metadata: {}", e);
                handler.on_progress(
                    "pixi",
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
                "pixi",
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
        "[pixi] Loaded {} package records in {:?}",
        total_records, repodata_elapsed
    );
    handler.on_progress(
        "pixi",
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
        "pixi",
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
                "pixi",
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
        "[pixi] Solved: {} packages to install in {:?}",
        required_packages.len(),
        solve_elapsed
    );
    handler.on_progress(
        "pixi",
        EnvProgressPhase::SolveComplete {
            package_count: required_packages.len(),
            elapsed_ms: solve_elapsed.as_millis() as u64,
        },
    );

    // Install
    handler.on_progress(
        "pixi",
        EnvProgressPhase::Installing {
            total: required_packages.len(),
        },
    );

    let reporter = RattlerReporter::new_with_env_type(handler.clone(), "pixi");
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
                "pixi",
                EnvProgressPhase::Error {
                    message: error_msg.clone(),
                },
            );
            return Err(anyhow!(error_msg));
        }
    }

    let install_elapsed = install_start.elapsed();
    info!(
        "[pixi] Environment ready at {:?} (install took {:?})",
        env_path, install_elapsed
    );
    handler.on_progress(
        "pixi",
        EnvProgressPhase::InstallComplete {
            elapsed_ms: install_elapsed.as_millis() as u64,
        },
    );

    Ok(())
}

/// Warm up a pixi environment by running Python to trigger .pyc compilation.
pub async fn warmup_environment(env: &PixiEnvironment) -> Result<()> {
    let warmup_start = Instant::now();
    info!("[pixi] Warming up environment at {:?}", env.venv_path);

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
        warn!("[pixi] Warmup failed for {:?}: {}", env.venv_path, stderr);
        return Ok(());
    }

    let marker_path = env.venv_path.join(".warmed");
    tokio::fs::write(&marker_path, "").await.ok();

    info!(
        "[pixi] Warmup complete for {:?} in {}ms",
        env.venv_path,
        warmup_start.elapsed().as_millis()
    );

    Ok(())
}

/// Check if a pixi environment has been warmed up.
pub fn is_environment_warmed(env: &PixiEnvironment) -> bool {
    env.venv_path.join(".warmed").exists()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_pixi_manifest_basic() {
        let manifest = generate_pixi_manifest(
            "test-project",
            &[
                "ipykernel".to_string(),
                "ipywidgets".to_string(),
                "numpy>=1.24".to_string(),
            ],
            &["conda-forge".to_string()],
        );

        assert!(manifest.contains("[workspace]"));
        assert!(manifest.contains("name = \"test-project\""));
        assert!(manifest.contains("\"conda-forge\""));
        assert!(manifest.contains("ipykernel = \"*\""));
        assert!(manifest.contains("ipywidgets = \"*\""));
        assert!(manifest.contains("numpy = \">=1.24\""));
        assert!(manifest.contains("[dependencies]"));
        assert!(manifest.contains("[tasks]"));
    }

    #[test]
    fn test_generate_pixi_manifest_multiple_channels() {
        let manifest = generate_pixi_manifest(
            "test",
            &["pandas".to_string()],
            &["conda-forge".to_string(), "defaults".to_string()],
        );

        assert!(manifest.contains("\"conda-forge\", \"defaults\""));
    }

    #[test]
    fn test_generate_pixi_manifest_version_specs() {
        let manifest = generate_pixi_manifest(
            "test",
            &[
                "numpy>=1.24".to_string(),
                "pandas<2.0".to_string(),
                "scipy==1.11".to_string(),
                "matplotlib!=3.7".to_string(),
            ],
            &["conda-forge".to_string()],
        );

        assert!(manifest.contains("numpy = \">=1.24\""));
        assert!(manifest.contains("pandas = \"<2.0\""));
        assert!(manifest.contains("scipy = \"==1.11\""));
        assert!(manifest.contains("matplotlib = \"!=3.7\""));
    }
}
