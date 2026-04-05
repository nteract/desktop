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
