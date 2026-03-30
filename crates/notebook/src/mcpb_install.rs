//! Build and install the nteract .mcpb (Claude Desktop extension) from the running app.
//!
//! Creates a `.mcpb` ZIP archive at runtime from embedded assets (manifest,
//! icons, launcher script), then opens it with the system handler so Claude
//! Desktop shows the install prompt.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use runt_workspace::{build_channel, BuildChannel};

/// The launcher script embedded at compile time.
const LAUNCH_JS: &str = include_str!("../../../mcpb/server/launch.js");

/// App icons embedded at compile time (both 512x512 PNG).
const ICON_STABLE: &[u8] = include_bytes!("../icons/icon.png");
const ICON_NIGHTLY: &[u8] = include_bytes!("../icons/icon-nightly.png");

/// Build a .mcpb archive and open it with the system handler.
///
/// Returns the path to the created .mcpb file on success.
pub fn install_mcpb(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let is_nightly = build_channel() == BuildChannel::Nightly;
    let version = env!("CARGO_PKG_VERSION");

    // Ensure the CLI is installed so runt/runt-nightly is on PATH.
    let cli_name = if is_nightly { "runt-nightly" } else { "runt" };
    if !cli_on_path(cli_name) {
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

    // ── 5. Write icons (pre-sized 512x512, no runtime resize needed) ──
    let (light_icon, dark_icon) = if is_nightly {
        (ICON_NIGHTLY, ICON_STABLE)
    } else {
        (ICON_STABLE, ICON_NIGHTLY)
    };
    fs::write(staging.join("icon.png"), light_icon).map_err(|e| {
        cleanup();
        format!("Failed to write icon.png: {e}")
    })?;
    fs::write(staging.join("icon-dark.png"), dark_icon).map_err(|e| {
        cleanup();
        format!("Failed to write icon-dark.png: {e}")
    })?;

    // ── 6. Create ZIP (.mcpb) ──────────────────────────────────────────
    let mcpb_name = if is_nightly {
        "nteract-nightly.mcpb"
    } else {
        "nteract.mcpb"
    };
    let mcpb_path = std::env::temp_dir().join(mcpb_name);
    let _ = fs::remove_file(&mcpb_path);

    // zip top-level files (manifest, icons) flat
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

    // Add server/ subdirectory preserving structure
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

    // ── 7. Open with system handler ────────────────────────────────────
    log::info!("[mcpb] Opening {}", mcpb_path.display());
    open_file(&mcpb_path)?;

    Ok(mcpb_path)
}

/// Check if a CLI command is on PATH.
fn cli_on_path(name: &str) -> bool {
    let cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    Command::new(cmd)
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Open a file with the platform's default handler.
fn open_file(path: &std::path::Path) -> Result<(), String> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    };
    Command::new(cmd)
        .arg(path)
        .spawn()
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    Ok(())
}
