//! CLI installation module for symlinking the bundled runt binary to PATH
//! and creating the channel-specific notebook shorthand wrapper script.
//!
//! On Unix systems, we create a symlink so the CLI automatically stays in sync
//! when the app is updated. On Windows, we copy the binary since symlinks
//! require admin privileges and have compatibility issues.

use runt_workspace::{cli_command_name, cli_notebook_alias_name};
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Command;
use tauri::Manager;

/// The directory where CLI tools are installed
#[cfg(target_os = "macos")]
const INSTALL_DIR: &str = "/usr/local/bin";

#[cfg(target_os = "linux")]
const INSTALL_DIR: &str = "/usr/local/bin";

#[cfg(target_os = "windows")]
const INSTALL_DIR: &str = ""; // Windows uses different mechanism

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

/// Check if the CLI is already installed
pub fn is_cli_installed() -> bool {
    let cli_path = PathBuf::from(INSTALL_DIR).join(cli_command_name());
    let nb_path = PathBuf::from(INSTALL_DIR).join(cli_notebook_alias_name());
    cli_path.exists() && nb_path.exists()
}

/// Install the CLI to the system PATH
/// Returns Ok(()) on success, Err with message on failure
pub fn install_cli(app: &tauri::AppHandle) -> Result<(), String> {
    let bundled_runt = get_bundled_runt_path(app)
        .ok_or_else(|| "Could not find bundled runt binary".to_string())?;

    let install_dir = PathBuf::from(INSTALL_DIR);
    let runt_dest = install_dir.join(cli_command_name());
    let nb_dest = install_dir.join(cli_notebook_alias_name());

    // Try direct copy first
    match try_install_direct(&bundled_runt, &runt_dest, &nb_dest) {
        Ok(()) => {
            log::info!("[cli_install] CLI installed successfully via direct copy");
            return Ok(());
        }
        Err(e) => {
            log::debug!(
                "[cli_install] Direct install failed: {}, trying with admin privileges",
                e
            );
        }
    }

    // Fall back to admin privileges
    #[cfg(target_os = "macos")]
    {
        install_with_admin_privileges(&bundled_runt, &runt_dest, &nb_dest)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("Permission denied. Please run with administrator privileges.".to_string())
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

/// Install using macOS admin privileges via osascript
#[cfg(target_os = "macos")]
fn install_with_admin_privileges(
    bundled_runt: &std::path::Path,
    runt_dest: &std::path::Path,
    nb_dest: &std::path::Path,
) -> Result<(), String> {
    // Write nb wrapper to a temp file (no admin needed for temp dir),
    // reusing create_nb_wrapper to avoid duplicating the script content.
    let temp_nb =
        std::env::temp_dir().join(format!("{}-install-script", cli_notebook_alias_name()));
    create_nb_wrapper(&temp_nb, cli_command_name())?;

    // Build shell commands -- create symlink for runt, copy nb wrapper script.
    // Using symlink ensures the CLI stays in sync when the app updates.
    // This avoids escaping issues with AppleScript string parsing.
    let commands = format!(
        "rm -f '{}' && ln -sf '{}' '{}' && cp '{}' '{}' && chmod 755 '{}'",
        runt_dest.display(),
        bundled_runt.display(),
        runt_dest.display(),
        temp_nb.display(),
        nb_dest.display(),
        nb_dest.display()
    );

    let script = format!(
        r#"do shell script "{}" with administrator privileges"#,
        commands.replace('\\', "\\\\").replace('"', "\\\"")
    );

    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("Failed to run osascript: {}", e))?;

    // Clean up temp file regardless of outcome
    let _ = fs::remove_file(&temp_nb);

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User canceled") || stderr.contains("(-128)") {
            Err("Installation cancelled by user.".to_string())
        } else {
            Err(format!("Installation failed: {}", stderr))
        }
    }
}
