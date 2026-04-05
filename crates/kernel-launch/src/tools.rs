//! Tool bootstrapping via rattler and direct downloads.
//!
//! This module provides a way to automatically install CLI tools (like `ruff`, `deno`, `uv`)
//! on demand. Tools are cached in `~/.cache/runt/tools/`.
//!
//! For Deno specifically, we download directly from GitHub releases for better reliability,
//! with a fallback to conda-forge via rattler.

use anyhow::{anyhow, Result};
use log::info;
use rattler::{default_cache_dir, install::Installer, package_cache::PackageCache};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions, Platform,
};
use rattler_repodata_gateway::Gateway;
use rattler_solve::{resolvo, SolverImpl, SolverTask};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::OnceCell;
use zip::ZipArchive;

/// Target Deno version for GitHub download.
pub const DENO_TARGET_VERSION: &str = "2.7.1";

/// Target UV version for GitHub download.
pub const UV_TARGET_VERSION: &str = "0.10.8";

/// Target pixi version for rattler bootstrap.
/// Pinned to ensure stable `pixi info --json` and `pixi shell-hook --json` output.
pub const PIXI_TARGET_VERSION: &str = "0.66";

/// Minimum acceptable Deno major version for system deno.
/// If system deno is below this version, we download a newer one.
pub const DENO_MIN_MAJOR_VERSION: u32 = 2;

/// Platform information for GitHub release assets (shared by Deno and UV).
struct GithubPlatform {
    arch: &'static str,
    platform: &'static str,
}

/// Cache directory for bootstrapped tools.
fn tools_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("tools")
}

/// Compute a hash for tool caching.
fn compute_tool_hash(tool_name: &str, version: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    if let Some(v) = version {
        hasher.update(b"=");
        hasher.update(v.as_bytes());
    }
    // Include platform in hash since binaries are platform-specific
    hasher.update(Platform::current().to_string().as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash)[..12].to_string()
}

/// Compute the binary path for a tool inside a given environment directory.
fn binary_path_for_env(env_path: &Path, tool_name: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        env_path.join("Scripts").join(format!("{}.exe", tool_name))
    }
    #[cfg(not(target_os = "windows"))]
    {
        env_path.join("bin").join(tool_name)
    }
}

/// Return the expected cached binary path for a tool/version without bootstrapping.
pub fn cached_tool_binary_path(tool_name: &str, version: Option<&str>) -> PathBuf {
    let hash = compute_tool_hash(tool_name, version);
    let env_path = tools_cache_dir().join(format!("{}-{}", tool_name, hash));
    binary_path_for_env(&env_path, tool_name)
}

/// Information about a bootstrapped tool.
#[derive(Debug, Clone)]
pub struct BootstrappedTool {
    /// Path to the tool binary
    pub binary_path: PathBuf,
    /// Path to the environment containing the tool
    pub env_path: PathBuf,
}

/// Bootstrap a tool from conda-forge.
///
/// This installs the tool into a cached environment and returns the path to the binary.
/// If the tool is already cached, returns immediately.
///
/// # Arguments
/// * `tool_name` - Name of the conda package (e.g., "ruff")
/// * `version` - Optional version constraint (e.g., "0.8")
///
/// # Example
/// ```ignore
/// let tool = bootstrap_tool("ruff", None).await?;
/// // Use tool.binary_path to run ruff
/// ```
pub async fn bootstrap_tool(tool_name: &str, version: Option<&str>) -> Result<BootstrappedTool> {
    let hash = compute_tool_hash(tool_name, version);
    let cache_dir = tools_cache_dir();
    let env_path = cache_dir.join(format!("{}-{}", tool_name, hash));
    let binary_path = binary_path_for_env(&env_path, tool_name);

    // Check if already cached
    if binary_path.exists() {
        info!("Using cached tool {} at {:?}", tool_name, binary_path);
        return Ok(BootstrappedTool {
            binary_path,
            env_path,
        });
    }

    info!("Bootstrapping {} via rattler...", tool_name);

    // Ensure cache directory exists
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Remove partial environment if it exists
    if env_path.exists() {
        tokio::fs::remove_dir_all(&env_path).await?;
    }

    // Setup channel configuration
    let channel_config = ChannelConfig::default_with_root_dir(cache_dir.clone());
    let channel = Channel::from_str("conda-forge", &channel_config)?;

    // Build spec for the tool
    let match_spec_options = ParseMatchSpecOptions::strict();
    let spec_str = match version {
        Some(v) => format!("{}={}", tool_name, v),
        None => tool_name.to_string(),
    };
    let spec = MatchSpec::from_str(&spec_str, match_spec_options)?;

    info!("Resolving {} from conda-forge...", spec_str);

    // Find or create the rattler cache directory
    let rattler_cache_dir = default_cache_dir()
        .map_err(|e| anyhow!("could not determine rattler cache directory: {}", e))?;
    rattler_cache::ensure_cache_dir(&rattler_cache_dir)
        .map_err(|e| anyhow!("could not create rattler cache directory: {}", e))?;

    // Create HTTP client for downloading
    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    // Create gateway for fetching repodata
    let gateway = Gateway::builder()
        .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(PackageCache::new(
            rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
        ))
        .with_client(download_client.clone())
        .finish();

    // Determine platforms to query
    let install_platform = Platform::current();
    let platforms = vec![install_platform, Platform::NoArch];

    // Query repodata
    let repo_data = gateway
        .query(vec![channel], platforms, vec![spec.clone()])
        .recursive(true)
        .await
        .map_err(|e| anyhow!("Failed to fetch repodata for {}: {}", tool_name, e))?;

    // Detect virtual packages
    let virtual_packages = rattler_virtual_packages::VirtualPackage::detect(
        &rattler_virtual_packages::VirtualPackageOverrides::default(),
    )?
    .iter()
    .map(|vpkg| GenericVirtualPackage::from(vpkg.clone()))
    .collect::<Vec<_>>();

    // Solve dependencies
    let solver_task = SolverTask {
        virtual_packages,
        specs: vec![spec],
        ..SolverTask::from_iter(&repo_data)
    };

    let solver_result = resolvo::Solver
        .solve(solver_task)
        .map_err(|e| anyhow!("Failed to solve {} dependencies: {}", tool_name, e))?;

    let required_packages = solver_result.records;
    info!(
        "Installing {} ({} packages)...",
        tool_name,
        required_packages.len()
    );

    // Install packages
    Installer::new()
        .with_download_client(download_client)
        .with_target_platform(install_platform)
        .install(&env_path, required_packages)
        .await
        .map_err(|e| anyhow!("Failed to install {}: {}", tool_name, e))?;

    // Verify the binary exists
    if !binary_path.exists() {
        return Err(anyhow!(
            "Tool {} was installed but binary not found at {:?}",
            tool_name,
            binary_path
        ));
    }

    info!(
        "Successfully bootstrapped {} at {:?}",
        tool_name, binary_path
    );

    Ok(BootstrappedTool {
        binary_path,
        env_path,
    })
}

/// Global cache for the ruff binary path.
/// This avoids repeated lookups once ruff is bootstrapped.
static RUFF_PATH: OnceCell<Arc<Result<PathBuf, String>>> = OnceCell::const_new();

/// Get the path to ruff, bootstrapping it if necessary.
///
/// This function:
/// 1. First checks if ruff is available on PATH (fast path)
/// 2. If not, bootstraps it via rattler from conda-forge
/// 3. Caches the result for subsequent calls
///
/// Returns the path to the ruff binary, or an error if it can't be obtained.
pub async fn get_ruff_path() -> Result<PathBuf> {
    let result = RUFF_PATH
        .get_or_init(|| async {
            // First, check if ruff is on PATH
            if let Ok(output) = tokio::process::Command::new("ruff")
                .arg("--version")
                .output()
                .await
            {
                if output.status.success() {
                    info!("Using system ruff");
                    return Arc::new(Ok(PathBuf::from("ruff")));
                }
            }

            // Not on PATH, bootstrap via rattler
            info!("ruff not found on PATH, bootstrapping via rattler...");
            match bootstrap_tool("ruff", None).await {
                Ok(tool) => Arc::new(Ok(tool.binary_path)),
                Err(e) => Arc::new(Err(e.to_string())),
            }
        })
        .await;

    match result.as_ref() {
        Ok(path) => Ok(path.clone()),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

/// Global cache for the deno binary path.
/// This avoids repeated lookups once deno is bootstrapped.
static DENO_PATH: OnceCell<Arc<Result<PathBuf, String>>> = OnceCell::const_new();

/// Check if a usable Deno is available without triggering a bootstrap.
///
/// Returns true if:
/// - System deno exists and is version 2.x+, OR
/// - A cached deno binary exists (either from GitHub download or rattler)
///
/// This is intended for UI availability checks where we don't want to
/// trigger a full download during initialization.
pub async fn check_deno_available_without_bootstrap() -> bool {
    // Check for acceptable system deno (2.x+)
    if let Ok(output) = tokio::process::Command::new("deno")
        .arg("--version")
        .output()
        .await
    {
        if output.status.success() {
            let version_str = String::from_utf8_lossy(&output.stdout);
            if let Some(major) = parse_deno_major_version(&version_str) {
                if major >= DENO_MIN_MAJOR_VERSION {
                    return true;
                }
            }
        }
    }

    // Check for cached GitHub download (versioned path)
    if cached_tool_binary_path("deno", Some(DENO_TARGET_VERSION)).exists() {
        return true;
    }

    // Check for cached rattler download (unversioned path, fallback)
    cached_tool_binary_path("deno", None).exists()
}

/// Get the GitHub release asset platform string for the current system.
fn get_github_platform() -> Result<GithubPlatform> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => return Err(anyhow!("Unsupported architecture: {}", other)),
    };

    let platform = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => return Err(anyhow!("Unsupported platform: {}", other)),
    };

    Ok(GithubPlatform { arch, platform })
}

/// Parse a version string and return the major version number.
/// Handles both "2.7.1" and "deno 2.7.1 (release, ...)" formats.
fn parse_deno_major_version(version_output: &str) -> Option<u32> {
    let line = version_output.lines().next()?;
    // Find the first token that starts with a digit (the version number)
    let version_str = line.split_whitespace().find(|s| {
        s.chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    })?;
    version_str.split('.').next()?.parse().ok()
}

/// Check if system deno is acceptable (major version >= 2).
/// Returns Some(path) if acceptable, None otherwise.
async fn check_system_deno_acceptable() -> Option<PathBuf> {
    let output = tokio::process::Command::new("deno")
        .arg("--version")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    let major = parse_deno_major_version(&version_str)?;

    if major >= DENO_MIN_MAJOR_VERSION {
        let version_line = version_str.lines().next().unwrap_or("unknown");
        info!("Using system deno ({})", version_line);
        Some(PathBuf::from("deno"))
    } else {
        info!(
            "System deno version {}.x is below minimum {}.x, will download newer version",
            major, DENO_MIN_MAJOR_VERSION
        );
        None
    }
}

/// Extract the deno binary from a zip archive.
fn extract_deno_zip(zip_bytes: &[u8], dest_dir: &Path) -> Result<PathBuf> {
    // Create destination directory structure
    let bin_dir = if cfg!(windows) {
        dest_dir.join("Scripts")
    } else {
        dest_dir.join("bin")
    };
    std::fs::create_dir_all(&bin_dir)?;

    // Open zip archive
    let cursor = Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(cursor)?;

    // The deno zip contains a single "deno" (or "deno.exe") file at the root
    let binary_name = if cfg!(windows) { "deno.exe" } else { "deno" };
    let mut extracted_path = None;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        // Only extract the deno binary
        if name == "deno" || name == "deno.exe" {
            let dest_path = bin_dir.join(binary_name);
            let mut dest_file = std::fs::File::create(&dest_path)?;
            std::io::copy(&mut file, &mut dest_file)?;
            extracted_path = Some(dest_path);
            break;
        }
    }

    extracted_path.ok_or_else(|| anyhow!("Deno binary not found in zip archive"))
}

/// Download and verify the deno binary from GitHub releases.
async fn download_deno_from_github(version: &str) -> Result<BootstrappedTool> {
    let platform = get_github_platform()?;
    let asset_name = format!("deno-{}-{}.zip", platform.arch, platform.platform);

    // Build URLs
    let zip_url = format!(
        "https://github.com/denoland/deno/releases/download/v{}/{}",
        version, asset_name
    );
    let checksum_url = format!("{}.sha256sum", zip_url);

    info!("Downloading deno {} from GitHub...", version);

    // Setup cache directory
    let cache_dir = tools_cache_dir();
    let hash = compute_tool_hash("deno", Some(version));
    let env_path = cache_dir.join(format!("deno-{}", hash));
    let binary_path = binary_path_for_env(&env_path, "deno");

    // Check if already cached
    if binary_path.exists() {
        info!("Using cached deno at {:?}", binary_path);
        return Ok(BootstrappedTool {
            binary_path,
            env_path,
        });
    }

    // Ensure cache directory exists
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Remove partial environment if it exists
    if env_path.exists() {
        tokio::fs::remove_dir_all(&env_path).await?;
    }

    // Create HTTP client (follows redirects automatically)
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    // Download checksum first
    info!("Fetching checksum from {}...", checksum_url);
    let checksum_response = client.get(&checksum_url).send().await?;
    if !checksum_response.status().is_success() {
        return Err(anyhow!(
            "Failed to download checksum: {}",
            checksum_response.status()
        ));
    }
    let checksum_text = checksum_response.text().await?;
    let expected_hash = checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("Invalid checksum format"))?
        .to_lowercase();

    // Download zip file
    info!("Downloading {}...", asset_name);
    let zip_response = client.get(&zip_url).send().await?;
    if !zip_response.status().is_success() {
        return Err(anyhow!(
            "Failed to download deno: {}",
            zip_response.status()
        ));
    }
    let zip_bytes = zip_response.bytes().await?;

    // Verify checksum
    info!("Verifying checksum...");
    let mut hasher = Sha256::new();
    hasher.update(&zip_bytes);
    let actual_hash = format!("{:x}", hasher.finalize());

    if actual_hash != expected_hash {
        return Err(anyhow!(
            "Checksum mismatch: expected {}, got {}",
            expected_hash,
            actual_hash
        ));
    }

    // Extract zip (blocking IO, run on blocking thread pool)
    info!("Extracting deno to {:?}...", env_path);
    let env_path_clone = env_path.clone();
    let binary_path_clone = binary_path.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let extracted_binary = extract_deno_zip(&zip_bytes, &env_path_clone)?;

        // Set executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&extracted_binary, perms)?;
        }

        // Silence unused warning on Windows where we don't set permissions
        let _ = &extracted_binary;

        // Verify binary exists at expected location
        if !binary_path_clone.exists() {
            return Err(anyhow!(
                "Deno binary not found after extraction at {:?}",
                binary_path_clone
            ));
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("Extraction task panicked: {}", e))??;

    info!(
        "Successfully installed deno {} at {:?}",
        version, binary_path
    );
    Ok(BootstrappedTool {
        binary_path,
        env_path,
    })
}

/// Get the path to deno, with the following priority:
///
/// 1. System deno if version >= 2.x (fast path, respects user's installation)
/// 2. Download from GitHub releases (v2.7.1) - most reliable source
/// 3. Fallback to rattler/conda-forge if GitHub download fails
///
/// Results are cached for subsequent calls.
pub async fn get_deno_path() -> Result<PathBuf> {
    let result = DENO_PATH
        .get_or_init(|| async {
            // 1. Check for acceptable system deno (2.x+)
            if let Some(path) = check_system_deno_acceptable().await {
                return Arc::new(Ok(path));
            }

            // 2. Try GitHub download (primary method)
            info!(
                "Downloading deno {} from GitHub releases...",
                DENO_TARGET_VERSION
            );
            match download_deno_from_github(DENO_TARGET_VERSION).await {
                Ok(tool) => return Arc::new(Ok(tool.binary_path)),
                Err(e) => {
                    info!("GitHub download failed: {}. Falling back to rattler...", e);
                }
            }

            // 3. Fallback to rattler
            info!("Bootstrapping deno via rattler from conda-forge...");
            match bootstrap_tool("deno", None).await {
                Ok(tool) => Arc::new(Ok(tool.binary_path)),
                Err(e) => Arc::new(Err(e.to_string())),
            }
        })
        .await;

    match result.as_ref() {
        Ok(path) => Ok(path.clone()),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

/// Extract the uv binary from a tar.gz archive.
fn extract_uv_tarball(tarball_bytes: &[u8], dest_dir: &Path) -> Result<PathBuf> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    // Create destination directory structure
    let bin_dir = if cfg!(windows) {
        dest_dir.join("Scripts")
    } else {
        dest_dir.join("bin")
    };
    std::fs::create_dir_all(&bin_dir)?;

    // Decompress and extract
    let decoder = GzDecoder::new(Cursor::new(tarball_bytes));
    let mut archive = Archive::new(decoder);

    // The uv tarball contains files in a directory like "uv-aarch64-apple-darwin/"
    // We need to find the uv binary and extract it
    let binary_name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let dest_path = bin_dir.join(binary_name);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        // Look for the uv binary (may be at root or in a subdirectory)
        if let Some(file_name) = path.file_name() {
            if file_name == binary_name {
                let mut dest_file = std::fs::File::create(&dest_path)?;
                std::io::copy(&mut entry, &mut dest_file)?;

                // Set executable permission on Unix
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o755);
                    std::fs::set_permissions(&dest_path, perms)?;
                }

                return Ok(dest_path);
            }
        }
    }

    Err(anyhow!("uv binary not found in tarball"))
}

/// Extract the uv binary from a zip archive (Windows).
#[cfg(target_os = "windows")]
fn extract_uv_zip(zip_bytes: &[u8], dest_dir: &Path) -> Result<PathBuf> {
    let bin_dir = dest_dir.join("Scripts");
    std::fs::create_dir_all(&bin_dir)?;

    let cursor = Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(cursor)?;

    let binary_name = "uv.exe";
    let dest_path = bin_dir.join(binary_name);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        if name.ends_with(binary_name) {
            let mut dest_file = std::fs::File::create(&dest_path)?;
            std::io::copy(&mut file, &mut dest_file)?;
            return Ok(dest_path);
        }
    }

    Err(anyhow!("uv.exe not found in zip archive"))
}

/// Download and verify the uv binary from GitHub releases.
async fn download_uv_from_github(version: &str) -> Result<BootstrappedTool> {
    let platform = get_github_platform()?;

    // UV uses tar.gz on Unix, zip on Windows
    let (asset_name, is_zip) = if cfg!(windows) {
        (
            format!("uv-{}-{}.zip", platform.arch, platform.platform),
            true,
        )
    } else {
        (
            format!("uv-{}-{}.tar.gz", platform.arch, platform.platform),
            false,
        )
    };

    // Build URLs
    let download_url = format!(
        "https://github.com/astral-sh/uv/releases/download/{}/{}",
        version, asset_name
    );
    let checksum_url = format!("{}.sha256", download_url);

    info!("Downloading uv {} from GitHub...", version);

    // Setup cache directory
    let cache_dir = tools_cache_dir();
    let hash = compute_tool_hash("uv", Some(version));
    let env_path = cache_dir.join(format!("uv-{}", hash));
    let binary_path = binary_path_for_env(&env_path, "uv");

    // Check if already cached
    if binary_path.exists() {
        info!("Using cached uv at {:?}", binary_path);
        return Ok(BootstrappedTool {
            binary_path,
            env_path,
        });
    }

    // Ensure cache directory exists
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Remove partial environment if it exists
    if env_path.exists() {
        tokio::fs::remove_dir_all(&env_path).await?;
    }

    // Create HTTP client
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    // Download checksum first
    info!("Fetching checksum from {}...", checksum_url);
    let checksum_response = client.get(&checksum_url).send().await?;
    if !checksum_response.status().is_success() {
        return Err(anyhow!(
            "Failed to download checksum: {}",
            checksum_response.status()
        ));
    }
    let checksum_text = checksum_response.text().await?;
    // UV checksums are just the hash (no filename)
    let expected_hash = checksum_text.trim().to_lowercase();

    // Download archive
    info!("Downloading {}...", asset_name);
    let archive_response = client.get(&download_url).send().await?;
    if !archive_response.status().is_success() {
        return Err(anyhow!(
            "Failed to download uv: {}",
            archive_response.status()
        ));
    }
    let archive_bytes = archive_response.bytes().await?;

    // Verify checksum
    info!("Verifying checksum...");
    let mut hasher = Sha256::new();
    hasher.update(&archive_bytes);
    let actual_hash = format!("{:x}", hasher.finalize());

    if actual_hash != expected_hash {
        return Err(anyhow!(
            "Checksum mismatch: expected {}, got {}",
            expected_hash,
            actual_hash
        ));
    }

    // Extract archive (blocking IO, run on blocking thread pool)
    info!("Extracting uv to {:?}...", env_path);
    let env_path_clone = env_path.clone();
    let binary_path_clone = binary_path.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if is_zip {
            #[cfg(target_os = "windows")]
            extract_uv_zip(&archive_bytes, &env_path_clone)?;
            #[cfg(not(target_os = "windows"))]
            return Err(anyhow!("Unexpected zip archive on non-Windows platform"));
        } else {
            extract_uv_tarball(&archive_bytes, &env_path_clone)?;
        }

        // Verify binary exists at expected location
        if !binary_path_clone.exists() {
            return Err(anyhow!(
                "uv binary not found after extraction at {:?}",
                binary_path_clone
            ));
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("Extraction task panicked: {}", e))??;

    info!("Successfully installed uv {} at {:?}", version, binary_path);
    Ok(BootstrappedTool {
        binary_path,
        env_path,
    })
}

/// Global cache for the uv binary path.
/// This avoids repeated lookups once uv is bootstrapped.
static UV_PATH: OnceCell<Arc<Result<PathBuf, String>>> = OnceCell::const_new();

/// Get the path to uv, with the following priority:
///
/// 1. System uv if available on PATH (fast path, respects user's installation)
/// 2. Download from GitHub releases (v0.10.8) - most reliable source
/// 3. Fallback to rattler/conda-forge if GitHub download fails
///
/// Results are cached for subsequent calls.
pub async fn get_uv_path() -> Result<PathBuf> {
    let result = UV_PATH
        .get_or_init(|| async {
            // 1. Check for system uv on PATH
            if let Ok(output) = tokio::process::Command::new("uv")
                .arg("--version")
                .output()
                .await
            {
                if output.status.success() {
                    info!("Using system uv");
                    return Arc::new(Ok(PathBuf::from("uv")));
                }
            }

            // 2. Try GitHub download (primary method)
            info!(
                "Downloading uv {} from GitHub releases...",
                UV_TARGET_VERSION
            );
            match download_uv_from_github(UV_TARGET_VERSION).await {
                Ok(tool) => return Arc::new(Ok(tool.binary_path)),
                Err(e) => {
                    info!("GitHub download failed: {}. Falling back to rattler...", e);
                }
            }

            // 3. Fallback to rattler
            info!("Bootstrapping uv via rattler from conda-forge...");
            match bootstrap_tool("uv", None).await {
                Ok(tool) => Arc::new(Ok(tool.binary_path)),
                Err(e) => Arc::new(Err(e.to_string())),
            }
        })
        .await;

    match result.as_ref() {
        Ok(path) => Ok(path.clone()),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

/// Global cache for the pixi binary path.
static PIXI_PATH: OnceCell<Arc<Result<PathBuf, String>>> = OnceCell::const_new();

/// Get the path to the pixi binary, checking system PATH first then bootstrapping.
///
/// Results are cached for subsequent calls.
pub async fn get_pixi_path() -> Result<PathBuf> {
    let result = PIXI_PATH
        .get_or_init(|| async {
            // 1. Check for system pixi on PATH
            if let Ok(output) = tokio::process::Command::new("pixi")
                .arg("--version")
                .output()
                .await
            {
                if output.status.success() {
                    info!("Using system pixi");
                    return Arc::new(Ok(PathBuf::from("pixi")));
                }
            }

            // 2. Bootstrap via rattler from conda-forge (pinned version)
            info!(
                "Bootstrapping pixi {} via rattler from conda-forge...",
                PIXI_TARGET_VERSION
            );
            match bootstrap_tool("pixi", Some(PIXI_TARGET_VERSION)).await {
                Ok(tool) => Arc::new(Ok(tool.binary_path)),
                Err(e) => Arc::new(Err(e.to_string())),
            }
        })
        .await;

    match result.as_ref() {
        Ok(path) => Ok(path.clone()),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

// ── Pixi project info via `pixi info --json` ────────────────────────

/// Environment info from `pixi info --json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PixiEnvironmentInfo {
    pub name: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub pypi_dependencies: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Project info from `pixi info --json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PixiProjectInfo {
    pub name: Option<String>,
    pub manifest_path: Option<String>,
    pub version: Option<String>,
}

/// Parsed result from `pixi info --json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PixiInfoResult {
    pub version: Option<String>,
    pub project_info: Option<PixiProjectInfo>,
    #[serde(default)]
    pub environments_info: Vec<PixiEnvironmentInfo>,
}

impl PixiInfoResult {
    /// Check if ipykernel is declared in the default environment's dependencies.
    pub fn has_ipykernel(&self) -> bool {
        self.environments_info.iter().any(|env| {
            env.name == "default"
                && (env.dependencies.iter().any(|d| d == "ipykernel")
                    || env.pypi_dependencies.iter().any(|d| d == "ipykernel"))
        })
    }

    /// Get the default environment's prefix path.
    pub fn default_prefix(&self) -> Option<&str> {
        self.environments_info
            .iter()
            .find(|e| e.name == "default")
            .and_then(|e| e.prefix.as_deref())
    }

    /// Get all dependency names from the default environment (sorted, for drift detection).
    pub fn default_deps_snapshot(&self) -> Vec<String> {
        let Some(env) = self.environments_info.iter().find(|e| e.name == "default") else {
            return Vec::new();
        };
        let mut deps: Vec<String> = env
            .dependencies
            .iter()
            .chain(env.pypi_dependencies.iter())
            .cloned()
            .collect();
        deps.sort();
        deps
    }
}

/// Run `pixi info --json` for a manifest path and parse the result.
///
/// The manifest can be either a `pixi.toml` or a `pyproject.toml` with `[tool.pixi]`.
pub async fn pixi_info(manifest_path: &std::path::Path) -> Result<PixiInfoResult> {
    let pixi_path = get_pixi_path().await?;
    let output = tokio::process::Command::new(&pixi_path)
        .args(["info", "--json", "--manifest-path"])
        .arg(manifest_path)
        .output()
        .await
        .map_err(|e| anyhow!("failed to run pixi info: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("pixi info failed: {}", stderr.trim()));
    }

    let result: PixiInfoResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("failed to parse pixi info JSON: {}", e))?;
    Ok(result)
}

/// Run `pixi shell-hook --json` and return the environment variables.
///
/// These env vars can be applied to a `Command` with `cmd.envs()` for
/// direct Python launch without the `pixi run` wrapper.
pub async fn pixi_shell_hook(
    manifest_path: &std::path::Path,
    environment: Option<&str>,
) -> Result<std::collections::HashMap<String, String>> {
    let pixi_path = get_pixi_path().await?;
    let mut cmd = tokio::process::Command::new(&pixi_path);
    cmd.args(["shell-hook", "--json", "--manifest-path"]);
    cmd.arg(manifest_path);
    if let Some(env_name) = environment {
        cmd.args(["--environment", env_name]);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow!("failed to run pixi shell-hook: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("pixi shell-hook failed: {}", stderr.trim()));
    }

    #[derive(serde::Deserialize)]
    struct ShellHookResult {
        environment_variables: std::collections::HashMap<String, String>,
    }

    let result: ShellHookResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("failed to parse pixi shell-hook JSON: {}", e))?;
    Ok(result.environment_variables)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_tool_hash() {
        let hash1 = compute_tool_hash("ruff", None);
        let hash2 = compute_tool_hash("ruff", Some("0.8"));
        let hash3 = compute_tool_hash("black", None);

        // Same tool/version should produce same hash
        assert_eq!(hash1, compute_tool_hash("ruff", None));

        // Different versions should produce different hashes
        assert_ne!(hash1, hash2);

        // Different tools should produce different hashes
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_tools_cache_dir() {
        let dir = tools_cache_dir();
        assert!(dir.to_string_lossy().contains("runt"));
        assert!(dir.to_string_lossy().contains("tools"));
    }

    #[test]
    fn test_compute_tool_hash_deno() {
        let hash1 = compute_tool_hash("deno", None);
        let hash2 = compute_tool_hash("deno", Some("2.0"));
        let hash_ruff = compute_tool_hash("ruff", None);

        // Same tool/version should produce same hash
        assert_eq!(hash1, compute_tool_hash("deno", None));

        // Different versions should produce different hashes
        assert_ne!(hash1, hash2);

        // Different tools should produce different hashes
        assert_ne!(hash1, hash_ruff);
    }

    #[test]
    fn test_compute_tool_hash_uv() {
        let hash1 = compute_tool_hash("uv", None);
        let hash2 = compute_tool_hash("uv", Some("0.10"));
        let hash_ruff = compute_tool_hash("ruff", None);

        // Same tool/version should produce same hash
        assert_eq!(hash1, compute_tool_hash("uv", None));

        // Different versions should produce different hashes
        assert_ne!(hash1, hash2);

        // Different tools should produce different hashes
        assert_ne!(hash1, hash_ruff);
    }

    #[test]
    fn test_parse_deno_major_version() {
        // Full version output format from `deno --version`
        assert_eq!(
            parse_deno_major_version("deno 2.7.1 (release, aarch64-apple-darwin)"),
            Some(2)
        );
        assert_eq!(
            parse_deno_major_version("deno 1.45.2 (release, x86_64-unknown-linux-gnu)"),
            Some(1)
        );
        assert_eq!(
            parse_deno_major_version("deno 2.0.0 (release, x86_64-pc-windows-msvc)"),
            Some(2)
        );

        // Simple version format
        assert_eq!(parse_deno_major_version("2.7.1"), Some(2));
        assert_eq!(parse_deno_major_version("1.0.0"), Some(1));
        assert_eq!(parse_deno_major_version("10.2.3"), Some(10));

        // Edge cases
        assert_eq!(parse_deno_major_version(""), None);
        assert_eq!(parse_deno_major_version("not a version"), None);
        assert_eq!(parse_deno_major_version("deno"), None);
    }

    #[test]
    fn test_get_github_platform() {
        let result = get_github_platform();
        // Should succeed on supported platforms (macOS, Linux, Windows on x86_64 or aarch64)
        #[cfg(any(
            all(target_arch = "aarch64", target_os = "macos"),
            all(target_arch = "x86_64", target_os = "macos"),
            all(target_arch = "aarch64", target_os = "linux"),
            all(target_arch = "x86_64", target_os = "linux"),
            all(target_arch = "x86_64", target_os = "windows"),
            all(target_arch = "aarch64", target_os = "windows"),
        ))]
        {
            assert!(result.is_ok());
            let platform = result.unwrap();
            assert!(!platform.arch.is_empty());
            assert!(!platform.platform.is_empty());
        }
    }

    #[test]
    fn test_deno_platform_mapping() {
        // Verify platform strings match GitHub release asset naming
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            let p = get_github_platform().unwrap();
            assert_eq!(p.arch, "aarch64");
            assert_eq!(p.platform, "apple-darwin");
        }

        #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
        {
            let p = get_github_platform().unwrap();
            assert_eq!(p.arch, "x86_64");
            assert_eq!(p.platform, "apple-darwin");
        }

        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        {
            let p = get_github_platform().unwrap();
            assert_eq!(p.arch, "x86_64");
            assert_eq!(p.platform, "unknown-linux-gnu");
        }

        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        {
            let p = get_github_platform().unwrap();
            assert_eq!(p.arch, "aarch64");
            assert_eq!(p.platform, "unknown-linux-gnu");
        }

        #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
        {
            let p = get_github_platform().unwrap();
            assert_eq!(p.arch, "x86_64");
            assert_eq!(p.platform, "pc-windows-msvc");
        }
    }

    #[test]
    fn test_version_constants() {
        // Ensure constants are sensible - version strings contain dots
        assert!(DENO_TARGET_VERSION.contains('.'));
        assert!(UV_TARGET_VERSION.contains('.'));
    }
}
