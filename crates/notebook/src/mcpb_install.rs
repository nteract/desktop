//! Build and install the nteract .mcpb (Claude Desktop extension) from the running app.
//!
//! Creates a `.mcpb` ZIP archive at runtime from:
//! - A manifest template (embedded) with the app's version and channel
//! - Icons from the app bundle
//! - A Node launcher script (embedded) as fallback entry_point
//!
//! Then opens it with the system handler (`open` on macOS), which triggers
//! Claude Desktop's extension install flow.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use runt_workspace::{build_channel, BuildChannel};

/// The launcher script embedded at compile time.
const LAUNCH_JS: &str = include_str!("../../../mcpb/server/launch.js");

/// Build a .mcpb archive and open it with the system handler.
///
/// Returns the path to the created .mcpb file on success.
/// On non-macOS platforms, returns an error (no `open` command).
pub fn install_mcpb(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        return Err("Extension installation is currently only supported on macOS.".into());
    }

    #[cfg(target_os = "macos")]
    install_mcpb_macos(app)
}

#[cfg(target_os = "macos")]
fn install_mcpb_macos(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let is_nightly = build_channel() == BuildChannel::Nightly;

    let version = env!("CARGO_PKG_VERSION");

    // Ensure the CLI is installed so runt/runt-nightly is on PATH.
    // This is needed because the mcpb's mcp_config.command references the binary directly.
    let cli_name = if is_nightly { "runt-nightly" } else { "runt" };
    if Command::new("which").arg(cli_name).output().map(|o| !o.status.success()).unwrap_or(true) {
        log::info!("[mcpb] CLI not found on PATH, installing first...");
        crate::cli_install::install_cli(app)?;
    }

    // ── 1. Build manifest ──────────────────────────────────────────────
    let (name, display_name, command) = if is_nightly {
        ("nteract-nightly", "nteract Nightly", "runt-nightly")
    } else {
        ("nteract", "nteract", "runt")
    };

    let manifest = serde_json::json!({
        "manifest_version": "0.3",
        "name": name,
        "display_name": display_name,
        "version": version,
        "description": "Create, edit, and run Jupyter notebooks with Claude",
        "author": {
            "name": "nteract contributors",
            "url": "https://nteract.io"
        },
        "repository": {
            "type": "git",
            "url": "https://github.com/nteract/desktop"
        },
        "license": "BSD-3-Clause",
        "server": {
            "type": "node",
            "entry_point": "server/launch.js",
            "mcp_config": {
                "command": command,
                "args": ["mcp"]
            }
        },
        "icon": "icon.png",
        "icons": [
            { "src": "icon.png", "size": "512x512", "theme": "light" },
            { "src": "icon-dark.png", "size": "512x512", "theme": "dark" }
        ],
        "keywords": ["jupyter", "notebook", "python", "data-science"]
    });

    let manifest_str = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {e}"))?;

    // ── 2. Create staging directory ────────────────────────────────────
    let staging = std::env::temp_dir().join(format!("nteract-mcpb-{}", std::process::id()));
    fs::create_dir_all(&staging).map_err(|e| format!("Failed to create staging directory: {e}"))?;

    // Clean up on error
    let cleanup = || {
        let _ = fs::remove_dir_all(&staging);
    };

    // ── 3. Write manifest ──────────────────────────────────────────────
    fs::write(staging.join("manifest.json"), &manifest_str).map_err(|e| {
        cleanup();
        format!("Failed to write manifest.json: {e}")
    })?;

    // ── 4. Write launcher script ───────────────────────────────────────
    let server_dir = staging.join("server");
    fs::create_dir_all(&server_dir).map_err(|e| {
        cleanup();
        format!("Failed to create server directory: {e}")
    })?;
    fs::write(server_dir.join("launch.js"), LAUNCH_JS).map_err(|e| {
        cleanup();
        format!("Failed to write launch.js: {e}")
    })?;

    // ── 5. Copy icons from app bundle ──────────────────────────────────
    let icon_copied = copy_app_icon(app, &staging, is_nightly);
    if !icon_copied {
        // Fallback: create a minimal 1x1 PNG placeholder
        log::warn!("[mcpb] Could not find app icons, using placeholder");
        let placeholder = create_placeholder_png();
        let _ = fs::write(staging.join("icon.png"), &placeholder);
        let _ = fs::write(staging.join("icon-dark.png"), &placeholder);
    }

    // ── 6. Create ZIP (.mcpb) ──────────────────────────────────────────
    let mcpb_name = if is_nightly {
        "nteract-nightly.mcpb"
    } else {
        "nteract.mcpb"
    };
    let mcpb_path = std::env::temp_dir().join(mcpb_name);

    // Remove existing .mcpb if present
    let _ = fs::remove_file(&mcpb_path);

    let status = Command::new("zip")
        .args(["-r", "-j"])
        .arg(&mcpb_path)
        .arg(&staging)
        .current_dir(&staging)
        .output()
        .map_err(|e| {
            cleanup();
            format!("Failed to run zip: {e}")
        })?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        cleanup();
        return Err(format!("zip failed: {stderr}"));
    }

    // Also need the server/ subdirectory in the zip
    let status = Command::new("zip")
        .args(["-r"])
        .arg(&mcpb_path)
        .arg("server/")
        .current_dir(&staging)
        .output()
        .map_err(|e| {
            cleanup();
            format!("Failed to add server/ to zip: {e}")
        })?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        cleanup();
        return Err(format!("zip (server/) failed: {stderr}"));
    }

    cleanup();

    // ── 7. Open the .mcpb with the system handler ──────────────────────
    log::info!("[mcpb] Opening {}", mcpb_path.display());
    Command::new("open")
        .arg(&mcpb_path)
        .spawn()
        .map_err(|e| format!("Failed to open .mcpb: {e}"))?;

    Ok(mcpb_path)
}

/// Try to copy icons from the app bundle into the staging directory.
/// Returns true if at least the light icon was copied.
///
/// On macOS, the app icon is at Contents/Resources/icon.icns. We extract
/// a 512x512 PNG from it using `sips`. If that fails, we look for any
/// bundled PNG icons (128x128, 32x32, etc.) and resize them.
#[cfg(target_os = "macos")]
fn copy_app_icon(app: &tauri::AppHandle, staging: &Path, _is_nightly: bool) -> bool {
    use tauri::Manager;

    let Ok(resource_dir) = app.path().resource_dir() else {
        return false;
    };

    // Strategy 1: Extract from .icns (always present in macOS app bundles)
    let icns_path = resource_dir.join("icon.icns");
    if icns_path.exists() {
        let dest = staging.join("icon.png");
        // sips can convert .icns to .png and resize in one step
        if resize_icon_sips(&icns_path, &dest) {
            // Use same icon for dark mode
            let _ = fs::copy(&dest, staging.join("icon-dark.png"));
            return true;
        }
    }

    // Strategy 2: Use any bundled PNG icon and resize
    for candidate in ["128x128@2x.png", "128x128.png", "32x32@2x.png"] {
        let path = resource_dir.join(candidate);
        if path.exists() {
            let dest = staging.join("icon.png");
            if resize_icon_sips(&path, &dest) {
                let _ = fs::copy(&dest, staging.join("icon-dark.png"));
                return true;
            }
        }
    }

    false
}

/// Resize an icon to 512x512 using macOS `sips`.
#[cfg(target_os = "macos")]
fn resize_icon_sips(src: &Path, dest: &Path) -> bool {
    Command::new("sips")
        .args([
            "-z",
            "512",
            "512",
            &src.to_string_lossy(),
            "--out",
            &dest.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a minimal valid 1x1 white PNG as a placeholder icon.
fn create_placeholder_png() -> Vec<u8> {
    // Minimal 1x1 white PNG
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
        0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
        0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, // compressed data
        0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, // checksum
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
        0xAE, 0x42, 0x60, 0x82,
    ]
}
