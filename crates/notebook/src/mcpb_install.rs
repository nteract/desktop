//! Build and install the nteract .mcpb (Claude Desktop extension) from the running app.
//!
//! Creates a `.mcpb` ZIP archive at runtime from:
//! - A manifest template (embedded) with the app's version and channel
//! - Icons from the app bundle
//! - A Node launcher script (embedded) as fallback entry_point
//!
//! Then opens it with the system handler (`open` on macOS), which triggers
//! Claude Desktop's extension install flow.

use std::path::PathBuf;

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
    macos::install_mcpb_macos(app)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use runt_workspace::{build_channel, BuildChannel};

    /// The launcher script embedded at compile time.
    const LAUNCH_JS: &str = include_str!("../../../mcpb/server/launch.js");

    /// App icons embedded at compile time (512x512 PNG).
    /// Stable = green/teal icon, Nightly = orange/red icon.
    const ICON_STABLE: &[u8] = include_bytes!("../icons/icon.png");
    const ICON_NIGHTLY_SRC: &[u8] = include_bytes!("../icons/source-nightly.png");

    pub fn install_mcpb_macos(app: &tauri::AppHandle) -> Result<PathBuf, String> {
        let is_nightly = build_channel() == BuildChannel::Nightly;

        let version = env!("CARGO_PKG_VERSION");

        // Ensure the CLI is installed so runt/runt-nightly is on PATH.
        // This is needed because the mcpb's mcp_config.command references the binary directly.
        let cli_name = if is_nightly { "runt-nightly" } else { "runt" };
        if Command::new("which")
            .arg(cli_name)
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
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
        fs::create_dir_all(&staging)
            .map_err(|e| format!("Failed to create staging directory: {e}"))?;

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

        // ── 5. Write embedded icons ──────────────────────────────────────────
        // Icons are embedded at compile time so they work in both dev and release builds.
        if is_nightly {
            // Nightly: light = nightly icon, dark = stable icon (swapped, matches xtask)
            // The nightly source is 1024x1024, resize to 512x512 via sips
            let light_tmp = staging.join("_nightly_src.png");
            fs::write(&light_tmp, ICON_NIGHTLY_SRC).map_err(|e| {
                cleanup();
                format!("Failed to write nightly icon: {e}")
            })?;
            let light_dest = staging.join("icon.png");
            if !resize_icon_sips(&light_tmp, &light_dest) {
                // If sips fails, use it as-is (Claude Desktop may accept any size)
                let _ = fs::rename(&light_tmp, &light_dest);
            } else {
                let _ = fs::remove_file(&light_tmp);
            }
            // Dark icon = stable icon (already 512x512)
            fs::write(staging.join("icon-dark.png"), ICON_STABLE).map_err(|e| {
                cleanup();
                format!("Failed to write dark icon: {e}")
            })?;
        } else {
            // Stable: light = stable icon, dark = nightly icon
            fs::write(staging.join("icon.png"), ICON_STABLE).map_err(|e| {
                cleanup();
                format!("Failed to write icon: {e}")
            })?;
            let dark_tmp = staging.join("_nightly_src.png");
            fs::write(&dark_tmp, ICON_NIGHTLY_SRC).map_err(|e| {
                cleanup();
                format!("Failed to write dark icon: {e}")
            })?;
            let dark_dest = staging.join("icon-dark.png");
            if !resize_icon_sips(&dark_tmp, &dark_dest) {
                let _ = fs::rename(&dark_tmp, &dark_dest);
            } else {
                let _ = fs::remove_file(&dark_tmp);
            }
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

    /// Resize an icon to 512x512 using macOS `sips`.
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
} // mod macos
