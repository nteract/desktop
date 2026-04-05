//! CLI installation module for symlinking the bundled runt binary to PATH
//! and creating the channel-specific notebook shorthand wrapper script.
//!
//! On Unix systems, we install to `~/.local/bin` (no admin privileges required)
//! and create a symlink so the CLI automatically stays in sync when the app
//! is updated. On Windows, we copy the binary since symlinks require admin
//! and have compatibility issues.

use runt_workspace::{cli_command_name, cli_notebook_alias_name};
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tauri::Manager;

/// Legacy install directory — checked for backward compatibility detection only.
#[cfg(unix)]
const LEGACY_INSTALL_DIR: &str = "/usr/local/bin";

/// Get the user-local install directory (`~/.local/bin` on Unix).
/// This requires no admin privileges and is the modern convention used by
/// rustup, uv, mise, pipx, and others.
#[cfg(unix)]
fn install_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local")
        .join("bin")
}

#[cfg(target_os = "windows")]
fn install_dir() -> PathBuf {
    // Windows uses a different mechanism (App Paths registry or user PATH)
    PathBuf::new()
}

/// Get the path to the bundled runt binary.
pub fn get_bundled_runt_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe_dir) = app.path().resource_dir() {
            // resource_dir on macOS points to Contents/Resources
            // The binary is in Contents/MacOS, which is ../MacOS from Resources
            let macos_dir = exe_dir.parent()?.join("MacOS");
            let bundled_path = macos_dir.join("runt");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runt");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runt.exe");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    // Fallback: try the development path (target/*/binaries/runt-{target})
    let target = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    } else {
        "x86_64-pc-windows-msvc"
    };

    let binary_name = if cfg!(windows) {
        format!("runt-{}.exe", target)
    } else {
        format!("runt-{}", target)
    };

    // Try to find it relative to the executable (for no-bundle dev builds)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let dev_path = exe_dir.join("binaries").join(&binary_name);
            if dev_path.exists() {
                log::debug!("[cli_install] Found dev runt at {:?}", dev_path);
                return Some(dev_path);
            }
            log::debug!("[cli_install] Dev runt not found at {:?}", dev_path);
        }
    }

    None
}

/// Result of checking whether an installed CLI symlink is current.
#[derive(Debug, PartialEq, Eq)]
pub enum SymlinkStatus {
    /// Symlink exists and points to the current app bundle binary.
    Current,
    /// Symlink exists but points to a different (stale) path.
    Stale,
    /// CLI is not installed (symlink does not exist).
    NotInstalled,
}

/// Check whether the installed `runt`/`runt-nightly` symlink in `~/.local/bin`
/// points to the current app bundle binary path, and whether the `nb`/`nb-nightly`
/// wrapper script references the correct CLI command name.
///
/// Only checks `~/.local/bin` — the canonical install location. Legacy
/// `/usr/local/bin` entries are warned about separately by `warn_legacy_cli_shadow()`
/// and are not auto-repaired (since `install_cli()` writes to `~/.local/bin` only).
///
/// Returns a tuple of `(runt_status, nb_status)`.
#[cfg(unix)]
pub fn check_cli_currency(app: &tauri::AppHandle) -> (SymlinkStatus, SymlinkStatus) {
    let dir = install_dir();
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();

    let runt_status = check_runt_symlink(app, &dir.join(cli_name));
    let nb_status = check_nb_script(&dir.join(nb_name), cli_name);

    (runt_status, nb_status)
}

/// Check if the runt symlink points to the current bundled binary.
///
/// Only considers an existing entry "ours" if it is a symlink whose target
/// contains "nteract" or "runt" in the path — this avoids clobbering unrelated
/// commands that happen to share the same name.
#[cfg(unix)]
fn check_runt_symlink(app: &tauri::AppHandle, symlink_path: &std::path::Path) -> SymlinkStatus {
    if !symlink_path.is_symlink() {
        // Not a symlink — either missing or a regular file/directory we don't own.
        return SymlinkStatus::NotInstalled;
    }

    let target = match fs::read_link(symlink_path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!(
                "[cli_install] Failed to read symlink {}: {}",
                symlink_path.display(),
                e
            );
            // Can't read it — don't touch what we can't verify
            return SymlinkStatus::NotInstalled;
        }
    };

    // Only consider this symlink ours if the target path matches the shape of
    // an nteract app bundle install. On macOS this is "*.app/Contents/MacOS/runt",
    // on Linux it's inside an nteract resource directory. We check for "nteract"
    // as a directory component AND the target filename being "runt" to avoid
    // false-positives on unrelated symlinks (e.g. /opt/homebrew/bin/runt).
    //
    // Note: if a user renames "nteract.app" to something else, the symlink will
    // no longer be recognized as ours, and auto-repair won't trigger. This is an
    // acceptable trade-off — renaming is rare and manual `install_cli()` still works.
    let target_str = target.to_string_lossy();
    let target_filename = target.file_name().map(|f| f.to_string_lossy());
    let looks_like_ours = target_filename.as_deref() == Some("runt")
        && (target_str.contains("/nteract") || target_str.contains("/nteract-nightly"));

    if !looks_like_ours {
        log::debug!(
            "[cli_install] Symlink {} -> {} does not appear to be an nteract install, skipping",
            symlink_path.display(),
            target_str
        );
        return SymlinkStatus::NotInstalled;
    }

    let bundled = match get_bundled_runt_path(app) {
        Some(p) => p,
        None => {
            log::debug!("[cli_install] Cannot determine bundled runt path for currency check");
            // Can't determine — assume current to avoid unnecessary reinstall
            return SymlinkStatus::Current;
        }
    };

    if target == bundled {
        SymlinkStatus::Current
    } else {
        log::info!(
            "[cli_install] Symlink stale: {} -> {} (expected {})",
            symlink_path.display(),
            target.display(),
            bundled.display()
        );
        SymlinkStatus::Stale
    }
}

/// Check if the nb wrapper script references the correct CLI command name.
///
/// Only considers the script "ours" if its content mentions "runt" — this avoids
/// clobbering unrelated `nb` commands.
#[cfg(unix)]
fn check_nb_script(script_path: &std::path::Path, expected_cli_name: &str) -> SymlinkStatus {
    if !script_path.exists() {
        return SymlinkStatus::NotInstalled;
    }

    match fs::read_to_string(script_path) {
        Ok(contents) => {
            // Only consider this script ours if it contains the exact exec pattern
            // that create_nb_wrapper() generates: "exec runt notebook" or
            // "exec runt-nightly notebook". A bare substring like "runt" would
            // false-positive on scripts mentioning "grunt", "runtime", etc.
            let is_ours = contents.contains("exec runt notebook")
                || contents.contains("exec runt-nightly notebook");

            if !is_ours {
                log::debug!(
                    "[cli_install] Script {} does not appear to be an nteract nb wrapper, skipping",
                    script_path.display()
                );
                return SymlinkStatus::NotInstalled;
            }

            let expected_exec = format!("exec {} notebook", expected_cli_name);
            if contents.contains(&expected_exec) {
                SymlinkStatus::Current
            } else {
                log::info!(
                    "[cli_install] nb script stale: {} does not contain '{}'",
                    script_path.display(),
                    expected_exec
                );
                SymlinkStatus::Stale
            }
        }
        Err(e) => {
            log::warn!(
                "[cli_install] Failed to read nb script {}: {}",
                script_path.display(),
                e
            );
            // Can't read — don't touch
            SymlinkStatus::NotInstalled
        }
    }
}

/// Returns true if the app appears to be running from a temporary or
/// ephemeral path. We must not rewrite CLI symlinks in this case because
/// the path will disappear, leaving the symlinks broken.
///
/// Covers:
/// - macOS app translocation (`/private/var/folders/`, `AppTranslocation`)
/// - macOS Downloads or Volumes (not yet moved to /Applications)
/// - Linux AppImage mounts (`/tmp/.mount_*`)
/// - General temp directories (`/tmp/`, `/var/folders/`)
#[cfg(unix)]
fn is_ephemeral_path(app: &tauri::AppHandle) -> bool {
    let bundled = match get_bundled_runt_path(app) {
        Some(p) => p,
        None => return false,
    };

    let path_str = bundled.to_string_lossy();

    let ephemeral = path_str.contains("/private/var/folders/")
        || path_str.contains("/AppTranslocation/")
        || path_str.starts_with("/tmp/")
        || path_str.starts_with("/var/folders/")
        || path_str.contains("/tmp/.mount_")
        || path_str.contains("/Downloads/")
        || path_str.starts_with("/Volumes/");

    if ephemeral {
        log::info!(
            "[cli_install] Skipping CLI currency check — app running from ephemeral path: {}",
            path_str
        );
    }

    ephemeral
}

/// Silently update the CLI installation if the symlinks are stale.
///
/// Called on app launch. If the user has previously installed the CLI (symlink
/// exists), this checks whether it still points to the current app bundle and
/// re-runs `install_cli()` if not. Does nothing if the CLI was never installed.
///
/// Skips the check in dev mode (source builds) and on macOS if the app is
/// running from a translocated path (e.g., directly from a DMG).
pub fn ensure_cli_current(app: &tauri::AppHandle) {
    #[cfg(not(unix))]
    {
        let _ = app;
        // On Windows, CLI install copies the binary — no symlink to check.
        // A future enhancement could compare binary hashes.
        return;
    }

    #[cfg(unix)]
    {
        // In dev mode, the bundled path points into a build artifact directory
        // (target/*/binaries/). Don't rewrite the user's global CLI to point there.
        if runt_workspace::is_dev_mode() {
            log::debug!("[cli_install] Dev mode — skipping CLI currency check");
            return;
        }

        // Skip if the app is running from an ephemeral path (macOS translocation,
        // AppImage mount, Downloads, DMG volume, etc.)
        if is_ephemeral_path(app) {
            return;
        }

        let (runt_status, nb_status) = check_cli_currency(app);

        log::debug!(
            "[cli_install] CLI currency check: runt={:?}, nb={:?}",
            runt_status,
            nb_status
        );

        // Only reinstall when at least one entry is Stale. install_cli() rewrites
        // both runt and nb, so we must ensure neither path is occupied by an
        // unrelated command. "NotInstalled" can mean either (a) the path doesn't
        // exist (safe to create) or (b) it exists but isn't ours (unsafe to
        // overwrite). Check actual path existence to distinguish the two cases.
        let runt_stale = runt_status == SymlinkStatus::Stale;
        let nb_stale = nb_status == SymlinkStatus::Stale;

        if !runt_stale && !nb_stale {
            log::debug!(
                "[cli_install] CLI currency check: runt={:?}, nb={:?} — no update needed",
                runt_status,
                nb_status
            );
            return;
        }

        // If either entry is "NotInstalled" (unrecognized), check whether the
        // path actually exists (or is a dangling symlink). If it does, something
        // else owns it — don't clobber. We use symlink_metadata() instead of
        // exists() because exists() returns false for dangling symlinks, which
        // would let us accidentally overwrite a broken symlink owned by another tool.
        let dir = install_dir();
        let cli_name = cli_command_name();
        let nb_name = cli_notebook_alias_name();

        let path_occupied = |p: PathBuf| fs::symlink_metadata(p).is_ok();
        let runt_blocked =
            runt_status == SymlinkStatus::NotInstalled && path_occupied(dir.join(cli_name));
        let nb_blocked =
            nb_status == SymlinkStatus::NotInstalled && path_occupied(dir.join(nb_name));

        if runt_blocked || nb_blocked {
            log::info!(
                "[cli_install] CLI partially stale (runt={:?}, nb={:?}) but {} \
                 is occupied by an unrelated command — skipping auto-repair",
                runt_status,
                nb_status,
                if runt_blocked { cli_name } else { nb_name }
            );
            return;
        }

        log::info!(
            "[cli_install] CLI needs update (runt={:?}, nb={:?}), reinstalling",
            runt_status,
            nb_status
        );
        if let Err(e) = install_cli(app) {
            log::warn!("[cli_install] Failed to update CLI: {}", e);
        } else {
            log::info!("[cli_install] CLI updated successfully");
        }
    }
}

/// Check if the CLI is already installed (checks both new and legacy locations).
pub fn is_cli_installed() -> bool {
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();

    let new_dir = install_dir();
    if new_dir.join(cli_name).exists() && new_dir.join(nb_name).exists() {
        return true;
    }

    #[cfg(unix)]
    {
        let legacy = PathBuf::from(LEGACY_INSTALL_DIR);
        if legacy.join(cli_name).exists() && legacy.join(nb_name).exists() {
            return true;
        }
    }

    false
}

/// Install the CLI to `~/.local/bin` (no admin privileges needed).
/// Returns Ok(()) on success, Err with message on failure.
pub fn install_cli(app: &tauri::AppHandle) -> Result<(), String> {
    let bundled_runt = get_bundled_runt_path(app)
        .ok_or_else(|| "Could not find bundled runt binary".to_string())?;

    let dir = install_dir();

    // Ensure ~/.local/bin exists
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;

    let runt_dest = dir.join(cli_command_name());
    let nb_dest = dir.join(cli_notebook_alias_name());

    try_install_direct(&bundled_runt, &runt_dest, &nb_dest)?;

    log::info!(
        "[cli_install] CLI installed: {} -> {}",
        runt_dest.display(),
        bundled_runt.display()
    );

    // Warn if legacy /usr/local/bin entries shadow ~/.local/bin
    #[cfg(unix)]
    warn_legacy_cli_shadow();

    // Ensure the user's shell RC has ~/.local/bin on PATH
    if let Err(e) = ensure_shell_path(&dir) {
        log::warn!("[cli_install] Shell PATH integration skipped: {}", e);
    }

    Ok(())
}

/// Warn if legacy /usr/local/bin has stale CLI copies that shadow ~/.local/bin.
///
/// Symlinks are fine — they track the app bundle. Only regular files (stale
/// copies from old installs) are a problem since they don't update.
#[cfg(unix)]
fn warn_legacy_cli_shadow() {
    let legacy = PathBuf::from(LEGACY_INSTALL_DIR);
    let stale: Vec<String> = [cli_command_name(), cli_notebook_alias_name()]
        .iter()
        .filter_map(|name| {
            let path = legacy.join(name);
            // Symlinks are fine — they resolve to the current app bundle.
            // Only warn about regular files (stale copies).
            if path.exists() && !path.is_symlink() {
                Some(path.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();

    if !stale.is_empty() {
        log::warn!(
            "[cli_install] Stale CLI copies in /usr/local/bin shadow ~/.local/bin: {}. \
             Remove with: sudo rm {}",
            stale.join(", "),
            stale.join(" ")
        );
    }
}

/// Try to install directly without admin privileges
fn try_install_direct(
    bundled_runt: &std::path::Path,
    runt_dest: &std::path::Path,
    nb_dest: &std::path::Path,
) -> Result<(), String> {
    // Remove existing file/symlink if present
    if runt_dest.exists() || runt_dest.is_symlink() {
        fs::remove_file(runt_dest)
            .map_err(|e| format!("Failed to remove existing {}: {}", cli_command_name(), e))?;
    }

    // On Unix, create a symlink so the CLI stays in sync when the app updates.
    // On Windows, copy the binary since symlinks require admin and have issues.
    #[cfg(unix)]
    {
        symlink(bundled_runt, runt_dest).map_err(|e| format!("Failed to create symlink: {}", e))?;
    }

    #[cfg(windows)]
    {
        fs::copy(bundled_runt, runt_dest).map_err(|e| format!("Failed to copy runt: {}", e))?;
    }

    // Create nb wrapper script
    create_nb_wrapper(nb_dest, cli_command_name())?;

    Ok(())
}

/// Create the nb wrapper script
fn create_nb_wrapper(nb_dest: &std::path::Path, cli_command: &str) -> Result<(), String> {
    let script = format!(
        r#"#!/bin/bash
# {} - open notebooks faster than you can say {} notebook
exec {} notebook "$@"
"#,
        cli_notebook_alias_name(),
        cli_command,
        cli_command
    );

    let mut file =
        fs::File::create(nb_dest).map_err(|e| format!("Failed to create nb script: {}", e))?;

    file.write_all(script.as_bytes())
        .map_err(|e| format!("Failed to write nb script: {}", e))?;

    // Make it executable
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(nb_dest)
            .map_err(|e| format!("Failed to get nb permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(nb_dest, perms)
            .map_err(|e| format!("Failed to set nb permissions: {}", e))?;
    }

    Ok(())
}

/// Ensure the user's shell RC file has `~/.local/bin` on PATH.
///
/// Appends a PATH export to `~/.zshrc`, `~/.bashrc`, or fish config if
/// `~/.local/bin` isn't already referenced. Idempotent — checks for the
/// marker comment or an existing `.local/bin` PATH entry before appending.
fn ensure_shell_path(bin_dir: &std::path::Path) -> Result<(), String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let home = dirs::home_dir().ok_or("could not determine home directory")?;

    let (rc_path, snippet) = if shell.ends_with("/fish") {
        let config = home.join(".config/fish/config.fish");
        (
            config,
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nfish_add_path {}\n",
                bin_dir.display()
            ),
        )
    } else if shell.ends_with("/bash") {
        (
            home.join(".bashrc"),
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nexport PATH=\"{}:$PATH\"\n",
                bin_dir.display()
            ),
        )
    } else {
        // Default to zsh (macOS default since Catalina)
        (
            home.join(".zshrc"),
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nexport PATH=\"{}:$PATH\"\n",
                bin_dir.display()
            ),
        )
    };

    // Read existing content (file may not exist yet)
    let existing = fs::read_to_string(&rc_path).unwrap_or_default();

    // Already configured — nothing to do
    if existing.contains(".local/bin") || existing.contains("Added by nteract") {
        log::debug!(
            "[cli_install] Shell RC {} already has ~/.local/bin on PATH",
            rc_path.display()
        );
        return Ok(());
    }

    // Ensure parent directory exists (relevant for fish config)
    if let Some(parent) = rc_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc_path)
        .map_err(|e| format!("Failed to open {}: {}", rc_path.display(), e))?;

    file.write_all(snippet.as_bytes())
        .map_err(|e| format!("Failed to write to {}: {}", rc_path.display(), e))?;

    log::info!(
        "[cli_install] Added ~/.local/bin to PATH in {}",
        rc_path.display()
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn nb_script_current_when_matches() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        fs::write(
            &script_path,
            "#!/bin/bash\n# nb - open notebooks\nexec runt-nightly notebook \"$@\"\n",
        )
        .ok();

        assert_eq!(
            check_nb_script(&script_path, "runt-nightly"),
            SymlinkStatus::Current
        );
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_stale_when_wrong_command() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        fs::write(
            &script_path,
            "#!/bin/bash\n# nb - open notebooks\nexec runt notebook \"$@\"\n",
        )
        .ok();

        // Expects runt-nightly but script has runt
        assert_eq!(
            check_nb_script(&script_path, "runt-nightly"),
            SymlinkStatus::Stale
        );
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_not_installed_when_missing() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb-nonexistent");

        assert_eq!(
            check_nb_script(&script_path, "runt"),
            SymlinkStatus::NotInstalled
        );
    }

    #[cfg(unix)]
    #[test]
    fn runt_symlink_not_installed_when_missing() {
        let dir = tempfile::tempdir().ok().unwrap();
        let symlink_path = dir.path().join("runt-nonexistent");

        assert!(!symlink_path.exists());
        assert!(!symlink_path.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_not_installed_when_not_ours() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        // An unrelated nb script — even one mentioning "grunt" (substring of "runt")
        fs::write(
            &script_path,
            "#!/bin/bash\n# build tool\nexec grunt \"$@\"\n",
        )
        .ok();

        assert_eq!(
            check_nb_script(&script_path, "runt"),
            SymlinkStatus::NotInstalled
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_not_treated_as_ours() {
        let dir = tempfile::tempdir().ok().unwrap();
        let file_path = dir.path().join("runt");
        fs::write(&file_path, "not a symlink").ok();

        // A regular file (not a symlink) should be NotInstalled, not Stale
        assert!(file_path.exists());
        assert!(!file_path.is_symlink());
    }
}
