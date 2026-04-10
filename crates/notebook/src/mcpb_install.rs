//! Build and install the nteract .mcpb (Claude Desktop extension) from the running app.
//!
//! Creates a `.mcpb` ZIP archive at runtime from embedded assets (manifest,
//! icons, mcpb-runt binary), then opens it with the system handler so Claude
//! Desktop shows the install prompt.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use runt_workspace::{build_channel, BuildChannel};

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
    let (name, display_name) = if is_nightly {
        ("nteract-nightly", "nteract Nightly")
    } else {
        ("nteract", "nteract")
    };

    let channel = if is_nightly { "nightly" } else { "stable" };

    let manifest = serde_json::json!({
        "manifest_version": "0.3",
        "name": name,
        "display_name": display_name,
        "version": version,
        "description": "Create, edit, and run Jupyter notebooks with Claude",
        "long_description": "nteract brings Jupyter notebooks to Claude. Create notebooks, execute Python and Deno code, visualize data with matplotlib/plotly/seaborn, manage dependencies, and see rich outputs — all from your Claude conversation.\n\nFeatures:\n- Create and open .ipynb notebooks\n- Execute code cells with inline output rendering\n- Manage Python dependencies (add, remove, sync)\n- Real-time presence — see Claude's cursor in the notebook app\n- Works with the nteract desktop app for side-by-side editing",
        "author": {
            "name": "nteract contributors",
            "url": "https://nteract.io"
        },
        "repository": {
            "type": "git",
            "url": "https://github.com/nteract/desktop"
        },
        "homepage": "https://nteract.io",
        "support": "https://github.com/nteract/desktop/issues",
        "license": "BSD-3-Clause",
        "server": {
            "type": "binary",
            "entry_point": "server/mcpb-runt",
            "mcp_config": {
                "command": "${__dirname}/server/mcpb-runt",
                "args": [],
                "env": { "NTERACT_CHANNEL": channel },
                "platform_overrides": {
                    "win32": {
                        "command": "${__dirname}/server/mcpb-runt.exe"
                    }
                }
            }
        },
        "tools": [
            { "name": "list_active_notebooks", "description": "List running notebook sessions." },
            { "name": "open_notebook", "description": "Open a notebook by path or session ID." },
            { "name": "create_notebook", "description": "Create a new notebook. Ephemeral by default; use save_notebook(path) to persist." },
            { "name": "save_notebook", "description": "Save notebook to disk." },
            { "name": "launch_app", "description": "Show the current notebook to the user." },
            { "name": "get_cell", "description": "Get a cell by ID." },
            { "name": "get_all_cells", "description": "Get all cells as summary, json, or rich format." },
            { "name": "create_cell", "description": "Create a cell, optionally executing it." },
            { "name": "set_cell", "description": "Replace a cell's source or type." },
            { "name": "delete_cell", "description": "Delete a cell." },
            { "name": "move_cell", "description": "Move a cell to a new position." },
            { "name": "clear_outputs", "description": "Clear cell outputs." },
            { "name": "execute_cell", "description": "Execute a code cell." },
            { "name": "run_all_cells", "description": "Execute all code cells in order." },
            { "name": "interrupt_kernel", "description": "Interrupt execution." },
            { "name": "restart_kernel", "description": "Restart the kernel, clearing all state." },
            { "name": "add_dependency", "description": "Add a package. Use after='sync' or 'restart' to apply." },
            { "name": "remove_dependency", "description": "Remove a package. Requires kernel restart to take effect." },
            { "name": "get_dependencies", "description": "Get the notebook's declared dependencies." },
            { "name": "sync_environment", "description": "Hot-install new dependencies. restart_kernel() if this fails." },
            { "name": "replace_match", "description": "Replace literal text in a cell. Use context_before/context_after to disambiguate repeated matches." },
            { "name": "replace_regex", "description": "Replace a regex match in a cell (fancy-regex). Fails if 0 or >1 matches. Replacement is literal text." },
            { "name": "add_cell_tags", "description": "Add tags to a cell." },
            { "name": "remove_cell_tags", "description": "Remove tags from a cell." },
            { "name": "set_cells_source_hidden", "description": "Hide or show cell source." },
            { "name": "set_cells_outputs_hidden", "description": "Hide or show cell outputs." }
        ],
        "tools_generated": false,
        "icon": "icon.png",
        "icons": [
            { "src": "icon.png", "size": "512x512", "theme": "light" },
            { "src": "icon-dark.png", "size": "512x512", "theme": "dark" }
        ],
        "compatibility": {
            "platforms": ["darwin", "win32", "linux"]
        },
        "keywords": ["jupyter", "notebook", "python", "deno", "data-science", "visualization"]
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

    // ── 4. Copy mcpb-runt binary from app sidecar ──────────────────────
    let server_dir = staging.join("server");
    fs::create_dir_all(&server_dir).map_err(|e| {
        cleanup();
        format!("Failed to create server directory: {e}")
    })?;

    let mcpb_runt_binary = get_bundled_mcpb_runt(app)?;
    let binary_name = if cfg!(target_os = "windows") {
        "mcpb-runt.exe"
    } else {
        "mcpb-runt"
    };
    fs::copy(&mcpb_runt_binary, server_dir.join(binary_name)).map_err(|e| {
        cleanup();
        format!("Failed to copy mcpb-runt binary: {e}")
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

/// Find the bundled `mcpb-runt` binary in the app's sidecar directory.
fn get_bundled_mcpb_runt(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;

    let binary_name = if cfg!(target_os = "windows") {
        "mcpb-runt.exe"
    } else {
        "mcpb-runt"
    };

    // Try the Tauri resource directory (where sidecars are bundled)
    if let Ok(resource_dir) = app.path().resource_dir() {
        let candidate = resource_dir.join(binary_name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // Fallback: look next to the main executable
    if let Ok(exe_dir) = std::env::current_exe().and_then(|p| {
        p.parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no parent"))
    }) {
        let candidate = exe_dir.join(binary_name);
        if candidate.exists() {
            return Ok(candidate);
        }

        // macOS: Contents/MacOS/mcpb-runt
        #[cfg(target_os = "macos")]
        {
            let candidate = exe_dir.join(binary_name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err("mcpb-runt binary not found in app bundle. \
         The app may need to be rebuilt with mcpb-runt as a sidecar."
        .to_string())
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
