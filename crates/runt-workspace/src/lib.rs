//! Workspace and dev mode utilities for Runt.
//!
//! This crate provides utilities for detecting development mode and managing
//! workspace-specific paths, enabling per-worktree isolation during development.

use sha2::{Digest, Sha256};
#[cfg(any(target_os = "macos", test))]
use std::ffi::OsString;
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

pub fn desktop_display_name_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "nteract",
        BuildChannel::Nightly => "nteract Nightly",
    }
}

pub fn cache_namespace_for(channel: BuildChannel) -> &'static str {
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

pub fn daemon_binary_basename_for(channel: BuildChannel) -> &'static str {
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

fn bundle_identifier_for(channel: BuildChannel) -> &'static str {
    match channel {
        BuildChannel::Stable => "org.nteract.desktop",
        BuildChannel::Nightly => "org.nteract.desktop.nightly",
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

/// macOS bundle identifier for the desktop app.
pub fn bundle_identifier() -> &'static str {
    bundle_identifier_for(build_channel())
}

/// All valid nteract bundle identifiers (stable + nightly).
///
/// Either is acceptable as the default `.ipynb` handler — a source-built
/// nightly `runt` should not warn about the stable app being the handler.
pub const NTERACT_BUNDLE_IDENTIFIERS: &[&str] =
    &["org.nteract.desktop", "org.nteract.desktop.nightly"];

/// Legacy bundle identifiers that should no longer be the default `.ipynb` handler.
pub const STALE_BUNDLE_IDENTIFIERS: &[&str] = &["com.runtimed.notebook"];

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

/// App names to try when launching the desktop notebook app for a given channel.
///
/// Returns candidates in priority order. On nightly, tries nightly-specific
/// names first, falling back to stable. On stable, only tries "nteract".
pub fn desktop_app_launch_candidates_for(channel: BuildChannel) -> &'static [&'static str] {
    match channel {
        BuildChannel::Stable => &["nteract"],
        BuildChannel::Nightly => &[
            "nteract Nightly",
            "nteract-nightly",
            "nteract (Nightly)", // legacy fallback for older installs
            "nteract",
        ],
    }
}

/// App names to try when launching the desktop notebook app.
pub fn desktop_app_launch_candidates() -> &'static [&'static str] {
    desktop_app_launch_candidates_for(build_channel())
}

/// Find the installed `.app` bundle on macOS for the current channel.
///
/// Searches `/Applications/` and `~/Applications/` for known app name candidates.
/// Returns the first match found.
#[cfg(target_os = "macos")]
pub fn find_installed_app_bundle() -> Option<PathBuf> {
    find_installed_app_bundle_for(build_channel())
}

/// Find the installed `.app` bundle on macOS for a specific channel.
#[cfg(target_os = "macos")]
pub fn find_installed_app_bundle_for(channel: BuildChannel) -> Option<PathBuf> {
    let home_apps = dirs::home_dir().map(|h| h.join("Applications"));
    for app_name in desktop_app_launch_candidates_for(channel) {
        let bundle_name = format!("{app_name}.app");
        let system = PathBuf::from("/Applications").join(&bundle_name);
        if system.exists() {
            return Some(system);
        }
        if let Some(ref home) = home_apps {
            let user = home.join(&bundle_name);
            if user.exists() {
                return Some(user);
            }
        }
    }
    None
}

/// Find any installed nteract app bundle and return its bundle identifier.
///
/// Tries the current channel first, then falls back to the other channel.
/// This ensures a source-built nightly `runt` finds the stable app if that's
/// what's installed.
#[cfg(target_os = "macos")]
pub fn find_any_installed_nteract_bundle() -> Option<(PathBuf, &'static str)> {
    let current = build_channel();
    let other = match current {
        BuildChannel::Stable => BuildChannel::Nightly,
        BuildChannel::Nightly => BuildChannel::Stable,
    };
    // Try current channel first
    if let Some(path) = find_installed_app_bundle_for(current) {
        return Some((path, bundle_identifier_for(current)));
    }
    // Fall back to other channel
    if let Some(path) = find_installed_app_bundle_for(other) {
        return Some((path, bundle_identifier_for(other)));
    }
    None
}

/// Launch the desktop notebook app for a specific channel.
///
/// In dev mode, uses the local bundled binary (channel is ignored — the local
/// binary is always used). In production, tries installed app candidates for
/// the given channel via platform-specific launch.
pub fn open_notebook_app_for_channel(
    channel: BuildChannel,
    path: Option<&Path>,
    extra_args: &[&str],
) -> Result<(), String> {
    if is_dev_mode() {
        return open_notebook_dev(path, extra_args);
    }
    open_notebook_installed_for(channel, path, extra_args)
}

/// Launch the desktop notebook app using the compile-time channel.
pub fn open_notebook_app(path: Option<&Path>, extra_args: &[&str]) -> Result<(), String> {
    open_notebook_app_for_channel(build_channel(), path, extra_args)
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

#[cfg(any(target_os = "macos", test))]
fn macos_open_args(path: Option<&Path>, extra_args: &[&str]) -> Vec<OsString> {
    let mut args = Vec::new();

    // Pass the notebook path as a document argument (before --args) so macOS
    // delivers it via Apple Events (kAEOpenDocuments) whether the app is
    // freshly launched or already running.
    if let Some(p) = path {
        args.push(p.as_os_str().to_os_string());
    }

    if !extra_args.is_empty() {
        args.push(OsString::from("--args"));
        args.extend(extra_args.iter().map(OsString::from));
    }

    args
}

fn open_notebook_installed_for(
    channel: BuildChannel,
    path: Option<&Path>,
    extra_args: &[&str],
) -> Result<(), String> {
    let mut last_error = None;

    for app_name in desktop_app_launch_candidates_for(channel) {
        #[cfg(target_os = "macos")]
        let spawn_result = {
            let mut cmd = Command::new("open");
            cmd.arg("-a").arg(app_name);
            cmd.args(macos_open_args(path, extra_args));
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
        desktop_display_name_for(channel),
        detail
    ))
}

// ============================================================================
// macOS Launch Services (File Associations)
// ============================================================================

#[cfg(target_os = "macos")]
pub mod launch_services {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use std::path::Path;
    use std::process::Command;

    type OSStatus = i32;
    type LSRolesMask = u32;
    #[allow(non_upper_case_globals)]
    const kLSRolesAll: LSRolesMask = 0xFFFFFFFF;

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(
            tag_class: CFStringRef,
            tag: CFStringRef,
            conforming_to: CFStringRef,
        ) -> CFStringRef;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: LSRolesMask,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: LSRolesMask,
            handler_bundle_id: CFStringRef,
        ) -> OSStatus;
    }

    /// Resolve the UTI for the `.ipynb` file extension.
    fn ipynb_uti() -> Option<CFString> {
        let ext = CFString::new("ipynb");
        unsafe {
            let uti = UTTypeCreatePreferredIdentifierForTag(
                kUTTagClassFilenameExtension,
                ext.as_concrete_TypeRef(),
                std::ptr::null(),
            );
            if uti.is_null() {
                return None;
            }
            Some(CFString::wrap_under_create_rule(uti))
        }
    }

    /// Query the default handler bundle ID for `.ipynb` files.
    ///
    /// Returns the lowercase bundle identifier of the app registered as the
    /// default handler, or `None` if no handler is set.
    pub fn get_default_ipynb_handler() -> Option<String> {
        let uti = ipynb_uti()?;
        unsafe {
            let handler =
                LSCopyDefaultRoleHandlerForContentType(uti.as_concrete_TypeRef(), kLSRolesAll);
            if handler.is_null() {
                return None;
            }
            let cf = CFString::wrap_under_create_rule(handler);
            Some(cf.to_string().to_lowercase())
        }
    }

    /// Set the default handler for `.ipynb` files to the given bundle ID.
    ///
    /// Returns `Ok(())` on success, or `Err(status)` with the OSStatus error code.
    pub fn set_default_ipynb_handler(bundle_id: &str) -> Result<(), i32> {
        let uti = ipynb_uti().ok_or(-1)?;
        let handler = CFString::new(bundle_id);
        let status = unsafe {
            LSSetDefaultRoleHandlerForContentType(
                uti.as_concrete_TypeRef(),
                kLSRolesAll,
                handler.as_concrete_TypeRef(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(status)
        }
    }

    /// Path to the `lsregister` tool in CoreServices framework.
    const LSREGISTER_PATH: &str = "/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister";

    /// Re-register an app bundle with Launch Services.
    ///
    /// This forces Launch Services to re-read the app's Info.plist and update
    /// its knowledge of file type claims. Equivalent to `lsregister -f <path>`.
    pub fn register_app_bundle(app_path: &Path) -> Result<(), String> {
        let output = Command::new(LSREGISTER_PATH)
            .args(["-f"])
            .arg(app_path)
            .output()
            .map_err(|e| format!("Failed to run lsregister: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("lsregister failed: {}", stderr.trim()))
        }
    }
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

    // launchd_bootstrap retries with backoff if launchd needs time to
    // clean up after the bootout above.
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
///
/// Retries with increasing delays on transient errors (e.g. error 5 —
/// I/O error) that occur when launchd hasn't fully cleaned up after a
/// recent `bootout`.
#[cfg(target_os = "macos")]
fn launchd_bootstrap(plist: &Path, domain: &str) -> Result<(), String> {
    // First attempt immediate, then backoff. Total max wait ~3.7s.
    let delays_ms: &[u64] = &[0, 200, 500, 1000, 2000];
    let mut last_err = String::new();

    for (attempt, &delay_ms) in delays_ms.iter().enumerate() {
        if delay_ms > 0 {
            eprintln!(
                "[launchd] bootstrap attempt {} after {}ms delay",
                attempt + 1,
                delay_ms,
            );
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }

        let output = Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain)
            .arg(plist)
            .output()
            .map_err(|e| format!("Failed to run launchctl bootstrap: {e}"))?;

        if output.status.success() {
            if attempt > 0 {
                eprintln!("[launchd] bootstrap succeeded on attempt {}", attempt + 1);
            }
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);

        // Error 37: service already loaded — that's fine
        if stderr.contains("37") {
            return Ok(());
        }

        // Error 5: I/O error — transient, launchd still cleaning up.
        // Match " 5: " precisely to avoid catching 15:, 25:, etc.
        if stderr.contains(" 5: ") || stderr.contains("Input/output") {
            last_err = format!("launchctl bootstrap failed: {}", stderr.trim());
            continue;
        }

        // Any other error — fail immediately
        return Err(format!("launchctl bootstrap failed: {}", stderr.trim()));
    }

    Err(last_err)
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

/// Read the workspace description from `.context/workspace-description`
/// in the worktree directory. Returns `None` if the file doesn't exist
/// or the worktree path is not set.
pub fn get_workspace_name() -> Option<String> {
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

/// Get the base directory for a specific channel's daemon context.
///
/// Like `daemon_base_dir()`, but accepts an explicit channel instead of
/// using the compile-time default. Useful for cross-channel discovery
/// (e.g., a stable-compiled binary finding the nightly socket).
pub fn daemon_base_dir_for(channel: BuildChannel) -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(cache_namespace_for(channel));

    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            let hash = worktree_hash(&worktree);
            return base.join("worktrees").join(hash);
        }
    }
    base
}

/// Get the default log path for the notebook app.
///
/// In dev mode: `~/.cache/runt/worktrees/{hash}/notebook.log`
/// Otherwise: `~/.cache/runt/notebook.log`
pub fn default_notebook_log_path() -> PathBuf {
    daemon_base_dir().join("notebook.log")
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

// ============================================================================
// Socket and Config Path Helpers
// ============================================================================

/// Get the default endpoint path for runtimed using the compile-time channel.
///
/// Respects `RUNTIMED_SOCKET_PATH` if set. Otherwise delegates to
/// `socket_path_for_channel(build_channel())`.
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = socket_path_from_env() {
        return path;
    }
    socket_path_for_channel(build_channel())
}

/// Get the endpoint path for a specific channel's daemon.
///
/// On Unix: `~/.cache/{namespace}/runtimed.sock` (or per-worktree in dev mode).
/// On Windows: `\\.\pipe\{daemon_name}` (with worktree hash suffix in dev mode).
///
/// Does **not** check `RUNTIMED_SOCKET_PATH` — that's an override for the
/// caller's own daemon, not for cross-channel discovery.
#[cfg(unix)]
pub fn socket_path_for_channel(channel: BuildChannel) -> PathBuf {
    daemon_base_dir_for(channel).join("runtimed.sock")
}

/// Get the endpoint path for a specific channel's daemon (Windows).
#[cfg(windows)]
pub fn socket_path_for_channel(channel: BuildChannel) -> PathBuf {
    let pipe_name = daemon_binary_basename_for(channel);
    if is_dev_mode() {
        if let Some(worktree) = get_workspace_path() {
            let hash = worktree_hash(&worktree);
            return PathBuf::from(format!(r"\\.\pipe\{}-{}", pipe_name, hash));
        }
    }
    PathBuf::from(format!(r"\\.\pipe\{}", pipe_name))
}

/// Check `RUNTIMED_SOCKET_PATH` env var and return the path if valid.
fn socket_path_from_env() -> Option<PathBuf> {
    let p = std::env::var("RUNTIMED_SOCKET_PATH").ok()?;
    let p = p.trim();
    if p.is_empty() {
        return None;
    }
    let path = PathBuf::from(p);
    if let Some(parent) = path.parent() {
        if parent.exists() {
            return Some(path);
        }
        panic!(
            "RUNTIMED_SOCKET_PATH directory does not exist: {}",
            parent.display()
        );
    }
    Some(path)
}

/// Get the path to the JSON settings file (for migration and fallback).
pub fn settings_json_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(config_namespace())
        .join("settings.json")
}

/// Get the path to the session state file.
///
/// In dev mode: stored per-worktree for isolation during development.
/// In production: stored in config directory alongside settings.
pub fn session_state_path() -> PathBuf {
    if is_dev_mode() {
        // Per-worktree session for dev isolation
        daemon_base_dir().join("session.json")
    } else {
        // Production: config directory
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(config_namespace())
            .join("session.json")
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

        assert_eq!(
            bundle_identifier_for(BuildChannel::Stable),
            "org.nteract.desktop"
        );
        assert_eq!(
            bundle_identifier_for(BuildChannel::Nightly),
            "org.nteract.desktop.nightly"
        );
    }

    #[test]
    fn test_macos_open_args_path_and_runtime() {
        let path = Path::new("/tmp/example.ipynb");
        let args = macos_open_args(Some(path), &["--runtime", "python"]);

        assert_eq!(
            args,
            vec![
                OsString::from("/tmp/example.ipynb"),
                OsString::from("--args"),
                OsString::from("--runtime"),
                OsString::from("python"),
            ]
        );
    }

    #[test]
    fn test_macos_open_args_path_only() {
        let path = Path::new("/tmp/example.ipynb");
        let args = macos_open_args(Some(path), &[]);

        assert_eq!(args, vec![OsString::from("/tmp/example.ipynb")]);
    }

    #[test]
    fn test_macos_open_args_runtime_only() {
        let args = macos_open_args(None, &["--runtime", "deno"]);

        assert_eq!(
            args,
            vec![
                OsString::from("--args"),
                OsString::from("--runtime"),
                OsString::from("deno"),
            ]
        );
    }
}
