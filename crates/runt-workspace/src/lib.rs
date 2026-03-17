//! Workspace and dev mode utilities for Runt.
//!
//! This crate provides utilities for detecting development mode and managing
//! workspace-specific paths, enabling per-worktree isolation during development.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Build Channel Branding
// ============================================================================

/// Release channel for this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildChannel {
    Stable,
    Nightly,
}

fn build_channel_from_str(channel: Option<&str>) -> BuildChannel {
    match channel {
        Some(value) if value.eq_ignore_ascii_case("stable") => BuildChannel::Stable,
        _ => BuildChannel::Nightly,
    }
}

/// Build channel for this binary.
///
/// Controlled by the compile-time `RUNT_BUILD_CHANNEL` environment variable.
/// Only `stable` is explicitly recognized; all other values (including unset)
/// default to `Nightly`. This ensures building from source is always nightly.
pub fn build_channel() -> BuildChannel {
    build_channel_from_str(option_env!("RUNT_BUILD_CHANNEL"))
}

fn desktop_product_name_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "nteract",
        BuildChannel::Nightly => "nteract-nightly",
    }
}

fn desktop_display_name_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "nteract",
        BuildChannel::Nightly => "nteract Nightly",
    }
}

fn cache_namespace_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "runt",
        BuildChannel::Nightly => "runt-nightly",
    }
}

fn config_namespace_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "nteract",
        BuildChannel::Nightly => "nteract-nightly",
    }
}

fn daemon_binary_basename_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "runtimed",
        BuildChannel::Nightly => "runtimed-nightly",
    }
}

fn daemon_service_basename_for(channel: BuildChannel) -> &'static str {
    daemon_binary_basename_for(channel)
}

fn daemon_launchd_label_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "io.nteract.runtimed",
        BuildChannel::Nightly => "io.nteract.runtimed.nightly",
    }
}

fn cli_command_name_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "runt",
        BuildChannel::Nightly => "runt-nightly",
    }
}

fn cli_notebook_alias_name_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "nb",
        BuildChannel::Nightly => "nb-nightly",
    }
}

/// Desktop product name used for package/app identity.
pub fn desktop_product_name() -> &'static str {
    desktop_product_name_for(build_channel())
}

/// Human-readable desktop app name.
pub fn desktop_display_name() -> &'static str {
    desktop_display_name_for(build_channel())
}

/// Channel-specific cache root directory name.
pub fn cache_namespace() -> &'static str {
    cache_namespace_for(build_channel())
}

/// Channel-specific config root directory name.
pub fn config_namespace() -> &'static str {
    config_namespace_for(build_channel())
}

/// Channel-specific daemon executable base name (without extension).
pub fn daemon_binary_basename() -> &'static str {
    daemon_binary_basename_for(build_channel())
}

/// Channel-specific daemon service base name.
pub fn daemon_service_basename() -> &'static str {
    daemon_service_basename_for(build_channel())
}

/// Channel-specific macOS launchd label for daemon service.
pub fn daemon_launchd_label() -> &'static str {
    daemon_launchd_label_for(build_channel())
}

/// Channel-specific CLI command name.
pub fn cli_command_name() -> &'static str {
    cli_command_name_for(build_channel())
}

/// Channel-specific shorthand notebook command name.
pub fn cli_notebook_alias_name() -> &'static str {
    cli_notebook_alias_name_for(build_channel())
}

/// Human-readable channel name for display.
pub fn channel_display_name() -> &'static str {
    match build_channel() {
        BuildChannel::Stable => "stable",
        BuildChannel::Nightly => "nightly",
    }
}

/// Context-aware guidance string for when the daemon is unavailable.
///
/// Returns appropriate instructions based on whether the user is in
/// dev mode (building from source) or running a released build.
pub fn daemon_unavailable_guidance() -> String {
    if is_dev_mode() {
        "Start the dev daemon with: cargo xtask dev-daemon".to_string()
    } else {
        format!(
            "Check daemon status with: {} daemon status",
            cli_command_name()
        )
    }
}

// ============================================================================
// Desktop App Launching
// ============================================================================

/// App names to try when launching the desktop notebook app.
///
/// Returns candidates in priority order. On nightly, tries nightly-specific
/// names first, falling back to stable. On stable, only tries "nteract".
pub fn desktop_app_launch_candidates() -> &'static [&'static str] {
    match build_channel() {
        BuildChannel::Stable => &["nteract"],
        BuildChannel::Nightly => &[
            "nteract Nightly",
            "nteract-nightly",
            "nteract (Nightly)", // legacy fallback for older installs
            "nteract",
        ],
    }
}

/// Launch the desktop notebook app, optionally opening a specific notebook.
///
/// In dev mode, uses the local bundled binary at `{workspace}/target/debug/notebook`.
/// Requires `cargo xtask build` first (checks for `.notebook-bundled` marker).
/// This ensures the app connects to the worktree daemon, not the system daemon.
///
/// In production, tries installed app candidates via platform-specific launch
/// (macOS: `open -a`, Linux/Windows: direct exec).
pub fn open_notebook_app(path: Option<&Path>, extra_args: &[&str]) -> Result<(), String> {
    if is_dev_mode() {
        return open_notebook_dev(path, extra_args);
    }
    open_notebook_installed(path, extra_args)
}

fn open_notebook_dev(path: Option<&Path>, extra_args: &[&str]) -> Result<(), String> {
    let workspace = get_workspace_path().ok_or("Dev mode active but no workspace path found")?;
    let binary_name = format!("notebook{}", std::env::consts::EXE_SUFFIX);
    let binary = workspace.join("target/debug").join(&binary_name);
    let marker = workspace.join("target/debug/.notebook-bundled");

    if !binary.exists() {
        return Err(format!(
            "No notebook binary found at {}. Run `cargo xtask build` first.",
            binary.display()
        ));
    }
    if !marker.exists() {
        return Err(format!(
            "Notebook binary at {} is a dev build (requires Vite server). \
             Run `cargo xtask build` for a standalone bundled binary.",
            binary.display()
        ));
    }

    let mut cmd = Command::new(&binary);
    if let Some(p) = path {
        cmd.arg(p);
    }
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to launch {}: {}", binary.display(), e))?;
    Ok(())
}

fn open_notebook_installed(path: Option<&Path>, extra_args: &[&str]) -> Result<(), String> {
    let mut last_error = None;

    for app_name in desktop_app_launch_candidates() {
        #[cfg(target_os = "macos")]
        let spawn_result = {
            let mut cmd = Command::new("open");
            cmd.arg("-a").arg(app_name);
            if path.is_some() || !extra_args.is_empty() {
                cmd.arg("--args");
            }
            if let Some(p) = path {
                cmd.arg(p);
            }
            for arg in extra_args {
                cmd.arg(arg);
            }
            cmd.spawn()
        };

        #[cfg(not(target_os = "macos"))]
        let spawn_result = {
            let mut cmd = Command::new(app_name);
            if let Some(p) = path {
                cmd.arg(p);
            }
            for arg in extra_args {
                cmd.arg(arg);
            }
            cmd.spawn()
        };

        match spawn_result {
            Ok(_) => return Ok(()),
            Err(e) => last_error = Some((app_name, e)),
        }
    }

    let detail = last_error
        .map(|(candidate, e)| format!("last attempt ({candidate}) failed: {e}"))
        .unwrap_or_else(|| "no launch candidates were attempted".to_string());
    Err(format!(
        "Failed to launch {}: {}",
        desktop_display_name(),
        detail
    ))
}

// ============================================================================
// macOS launchd Service Management
// ============================================================================

/// Get the current user's UID for the launchd gui domain.
#[cfg(target_os = "macos")]
pub fn launchd_uid() -> Result<String, String> {
    let output = Command::new("id")
        .args(["-u"])
        .output()
        .map_err(|e| format!("Failed to run `id -u`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`id -u` failed (exit {}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let uid = String::from_utf8(output.stdout)
        .map_err(|e| format!("Non-UTF8 output from `id -u`: {e}"))?;

    let uid = uid.trim().to_string();
    if uid.is_empty() {
        return Err("Empty UID from `id -u`".to_string());
    }

    Ok(uid)
}

/// Path to the launchd plist for the current channel.
#[cfg(target_os = "macos")]
pub fn launchd_plist_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(format!(
        "Library/LaunchAgents/{}.plist",
        daemon_launchd_label()
    )))
}

/// Stop the daemon's launchd service via `bootout`.
///
/// Ignores errors if the service is not currently loaded.
#[cfg(target_os = "macos")]
pub fn launchd_stop() -> Result<(), String> {
    let uid = launchd_uid()?;
    let label = daemon_launchd_label();
    let domain_target = format!("gui/{uid}/{label}");

    let output = Command::new("launchctl")
        .args(["bootout", &domain_target])
        .output()
        .map_err(|e| format!("Failed to run launchctl bootout: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not found" errors — service may not be loaded
        if !stderr.contains("Could not find")
            && !stderr.contains("No such")
            && !stderr.contains("36")
            && !stderr.contains("113")
        {
            return Err(format!("launchctl bootout failed: {}", stderr.trim()));
        }
    }

    Ok(())
}

/// Start the daemon's launchd service via `bootstrap`.
///
/// Clears any stale registration first via `launchd_stop()` (ignoring
/// "not found" errors), then bootstraps fresh from the plist.
#[cfg(target_os = "macos")]
pub fn launchd_start() -> Result<(), String> {
    let plist = launchd_plist_path()?;
    if !plist.exists() {
        return Err(format!("launchd plist not found at {}", plist.display()));
    }

    let uid = launchd_uid()?;
    let domain = format!("gui/{uid}");

    // Clear any stale registration — launchd_stop() already treats "not found"
    // as Ok, so ? propagates only unexpected failures.
    launchd_stop()?;

    // Brief pause for launchd to clean up
    std::thread::sleep(std::time::Duration::from_millis(100));

    launchd_bootstrap(&plist, &domain)
}

/// Check whether the daemon's launchd service is currently loaded.
#[cfg(target_os = "macos")]
pub fn launchd_is_loaded() -> Result<bool, String> {
    let label = daemon_launchd_label();
    let output = Command::new("launchctl")
        .args(["list", label])
        .output()
        .map_err(|e| format!("Failed to run launchctl list: {e}"))?;

    if output.status.success() {
        return Ok(true);
    }

    // Only treat "not found" as "not loaded"; surface unexpected errors
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Could not find")
        || stderr.contains("No such")
        || stderr.contains("36")
        || stderr.contains("113")
    {
        return Ok(false);
    }

    Err(format!("launchctl list failed: {}", stderr.trim()))
}

/// Ensure the daemon's launchd service is loaded, without restarting it
/// if it's already running.
///
/// Unlike `launchd_start()` which always does bootout+bootstrap (a restart),
/// this only bootstraps if the service is not currently loaded.
/// Returns `true` if it actually bootstrapped, `false` if already loaded.
#[cfg(target_os = "macos")]
pub fn launchd_ensure_loaded() -> Result<bool, String> {
    if launchd_is_loaded()? {
        return Ok(false);
    }
    let plist = launchd_plist_path()?;
    if !plist.exists() {
        return Err(format!("launchd plist not found at {}", plist.display()));
    }
    let uid = launchd_uid()?;
    let domain = format!("gui/{uid}");

    launchd_bootstrap(&plist, &domain)?;
    Ok(true)
}

/// Run `launchctl bootstrap` for the given plist in the given domain.
#[cfg(target_os = "macos")]
fn launchd_bootstrap(plist: &Path, domain: &str) -> Result<(), String> {
    let output = Command::new("launchctl")
        .arg("bootstrap")
        .arg(domain)
        .arg(plist)
        .output()
        .map_err(|e| format!("Failed to run launchctl bootstrap: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Error 37 means service is already loaded (which is fine)
        if !stderr.contains("37") {
            return Err(format!("launchctl bootstrap failed: {}", stderr.trim()));
        }
    }

    Ok(())
}

// ============================================================================
// Development Mode Detection
// ============================================================================

/// Check if development mode is enabled.
///
/// Dev mode enables per-worktree daemon isolation, allowing each git worktree
/// to run its own daemon instance with separate state directories.
///
/// Returns true if:
/// - `RUNTIMED_DEV=1` is set (explicit opt-in), OR
/// - `RUNTIMED_WORKSPACE_PATH` is set (explicit workspace isolation)
pub fn is_dev_mode() -> bool {
    // Explicit opt-in
    if std::env::var("RUNTIMED_DEV")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return true;
    }
    // Workspace path presence enables dev mode
    std::env::var("RUNTIMED_WORKSPACE_PATH").is_ok()
}

/// Get the workspace path for dev mode.
///
/// Uses `RUNTIMED_WORKSPACE_PATH` if available, otherwise detects via git.
pub fn get_workspace_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RUNTIMED_WORKSPACE_PATH") {
        return Some(PathBuf::from(path));
    }
    // Fallback to git detection
    detect_worktree_root()
}

/// Get the workspace name for display.
///
/// Uses `RUNTIMED_WORKSPACE_NAME` if available, otherwise reads from
/// `.context/workspace-description` file in the worktree.
pub fn get_workspace_name() -> Option<String> {
    if let Ok(name) = std::env::var("RUNTIMED_WORKSPACE_NAME") {
        return Some(name);
    }
    // Fallback: read .context/workspace-description
    get_workspace_path()
        .and_then(|p| std::fs::read_to_string(p.join(".context/workspace-description")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect the current git worktree root.
///
/// Runs `git rev-parse --show-toplevel` to find the root directory.
fn detect_worktree_root() -> Option<PathBuf> {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.exists())
}

/// Compute a short hash of a worktree path for directory naming.
///
/// Returns the first 12 hex characters of the SHA-256 hash.
pub fn worktree_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hex::encode(&hasher.finalize()[..6]) // 6 bytes = 12 hex chars
}

/// Derive a stable Vite dev server port for a worktree.
///
/// Uses the first 4 hex chars of the worktree hash to pick a port in
/// the range 5100–9999, avoiding conflicts between parallel worktrees.
/// Returns `None` if the workspace path can't be determined.
pub fn default_vite_port() -> Option<u16> {
    let workspace = get_workspace_path()?;
    Some(vite_port_for_workspace(&workspace))
}

/// Derive a Vite port for a specific workspace path.
pub fn vite_port_for_workspace(path: &Path) -> u16 {
    let hash = worktree_hash(path);
    let prefix = hash.get(..4).unwrap_or("0000");
    let offset = u16::from_str_radix(prefix, 16).unwrap_or(0) % 4900;
    5100 + offset
}

// ============================================================================
// Directory Paths
// ============================================================================

/// Get the base directory for the current daemon context.
///
/// In dev mode: `~/.cache/runt/worktrees/{hash}/`
/// Otherwise: `~/.cache/runt/`
pub fn daemon_base_dir() -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(cache_namespace());

    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            let hash = worktree_hash(&worktree);
            return base.join("worktrees").join(hash);
        }
    }
    base
}

/// Get the default directory for saving notebooks.
///
/// In dev mode with a workspace path: tries `{workspace}/notebooks/` first,
/// falling back to `~/notebooks/` if the workspace dir can't be created.
/// Otherwise: `~/notebooks/`
///
/// Creates the directory if it doesn't exist.
pub fn default_notebooks_dir() -> Result<PathBuf, String> {
    // In dev mode, try workspace notebooks first with fallback to home
    if is_dev_mode() {
        if let Some(workspace) = get_workspace_path() {
            let workspace_notebooks = workspace.join("notebooks");
            if ensure_dir_exists(&workspace_notebooks).is_ok() {
                return Ok(workspace_notebooks);
            }
            // Workspace dir failed (stale path, permissions, etc.) - fall back to ~/notebooks
            eprintln!(
                "Warning: Could not create {}, falling back to ~/notebooks",
                workspace_notebooks.display()
            );
        }
    }

    // Default to ~/notebooks
    let home_notebooks = home_notebooks_dir()?;
    ensure_dir_exists(&home_notebooks)?;
    Ok(home_notebooks)
}

/// Get the ~/notebooks directory path.
fn home_notebooks_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join("notebooks"))
}

/// Ensure a directory exists, creating it if necessary.
fn ensure_dir_exists(dir: &Path) -> Result<(), String> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if dir.is_dir() {
                Ok(())
            } else {
                Err(format!("{} exists but is not a directory", dir.display()))
            }
        }
        Err(e) => Err(format!(
            "Failed to create directory {}: {}",
            dir.display(),
            e
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_hash_consistency() {
        let path = Path::new("/some/path");
        let hash1 = worktree_hash(path);
        let hash2 = worktree_hash(path);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 12);
    }

    #[test]
    fn test_worktree_hash_differs() {
        let path1 = Path::new("/path/one");
        let path2 = Path::new("/path/two");
        assert_ne!(worktree_hash(path1), worktree_hash(path2));
    }

    #[test]
    fn test_build_channel_parsing() {
        // Unset defaults to nightly (building from source)
        assert_eq!(build_channel_from_str(None), BuildChannel::Nightly);
        // Only explicit "stable" gives stable
        assert_eq!(build_channel_from_str(Some("stable")), BuildChannel::Stable);
        assert_eq!(build_channel_from_str(Some("STABLE")), BuildChannel::Stable);
        // Everything else is nightly
        assert_eq!(
            build_channel_from_str(Some("nightly")),
            BuildChannel::Nightly
        );
        assert_eq!(
            build_channel_from_str(Some("something-else")),
            BuildChannel::Nightly
        );
    }

    #[test]
    fn test_desktop_app_launch_candidates() {
        let candidates = desktop_app_launch_candidates();
        // Should always have at least one candidate
        assert!(!candidates.is_empty());
        // Last candidate should always be "nteract" (the stable fallback)
        assert_eq!(*candidates.last().unwrap(), "nteract");
    }

    #[test]
    fn test_branding_matrix_values() {
        assert_eq!(desktop_product_name_for(BuildChannel::Stable), "nteract");
        assert_eq!(
            desktop_product_name_for(BuildChannel::Nightly),
            "nteract-nightly"
        );
        assert_eq!(desktop_display_name_for(BuildChannel::Stable), "nteract");
        assert_eq!(
            desktop_display_name_for(BuildChannel::Nightly),
            "nteract Nightly"
        );

        assert_eq!(cache_namespace_for(BuildChannel::Stable), "runt");
        assert_eq!(cache_namespace_for(BuildChannel::Nightly), "runt-nightly");

        assert_eq!(config_namespace_for(BuildChannel::Stable), "nteract");
        assert_eq!(
            config_namespace_for(BuildChannel::Nightly),
            "nteract-nightly"
        );

        assert_eq!(daemon_binary_basename_for(BuildChannel::Stable), "runtimed");
        assert_eq!(
            daemon_binary_basename_for(BuildChannel::Nightly),
            "runtimed-nightly"
        );

        assert_eq!(cli_command_name_for(BuildChannel::Stable), "runt");
        assert_eq!(cli_command_name_for(BuildChannel::Nightly), "runt-nightly");
        assert_eq!(cli_notebook_alias_name_for(BuildChannel::Stable), "nb");
        assert_eq!(
            cli_notebook_alias_name_for(BuildChannel::Nightly),
            "nb-nightly"
        );

        assert_eq!(
            daemon_launchd_label_for(BuildChannel::Stable),
            "io.nteract.runtimed"
        );
        assert_eq!(
            daemon_launchd_label_for(BuildChannel::Nightly),
            "io.nteract.runtimed.nightly"
        );
    }
}
