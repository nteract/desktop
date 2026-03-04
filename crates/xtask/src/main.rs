use std::env;
use std::fs;
use std::path::Path;
use std::process::{exit, Command};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        exit(0);
    }

    match args[0].as_str() {
        "dev" => {
            let attach = args.iter().any(|a| a == "--attach");
            let notebook = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .map(String::as_str);
            cmd_dev(notebook, attach);
        }
        "vite" => cmd_vite(),
        "build" => {
            let rust_only = args.iter().any(|a| a == "--rust-only");
            cmd_build(rust_only);
        }
        "run" => {
            let notebook = args.get(1).map(String::as_str);
            cmd_run(notebook);
        }
        "icons" => {
            let source = args.get(1).map(String::as_str);
            cmd_icons(source);
        }
        "build-e2e" => cmd_build_e2e(),
        "build-dmg" => cmd_build_dmg(),
        "build-app" => cmd_build_app(),
        "install-daemon" => cmd_install_daemon(),
        "dev-daemon" => cmd_dev_daemon(),
        "--help" | "-h" | "help" => print_help(),
        cmd => {
            eprintln!("Unknown command: {cmd}");
            eprintln!();
            print_help();
            exit(1);
        }
    }
}

fn print_help() {
    eprintln!(
        "Usage: cargo xtask <COMMAND>

Development:
  dev [notebook.ipynb]       Start hot-reload dev server (Vite + Tauri)
  dev --attach [notebook]    Attach Tauri to existing Vite server
  vite                       Start Vite server standalone
  build                      Full debug build (frontend + rust)
  build --rust-only          Rebuild rust only, reuse existing frontend
  build-e2e                  Debug build with built-in WebDriver server
  run [notebook.ipynb]       Run bundled debug binary

Release:
  build-app                  Build .app bundle with icons
  build-dmg                  Build DMG with icons (for CI)

Daemon:
  install-daemon             Build and install runtimed into the running service
  dev-daemon                 Build and run runtimed in per-worktree dev mode

Other:
  icons [source.png]         Generate icon variants
  help                       Show this help
"
    );
}

fn cmd_dev(notebook: Option<&str>, attach: bool) {
    // Delete bundled marker since we're building a dev binary
    let marker = Path::new("./target/debug/.notebook-bundled");
    let _ = fs::remove_file(marker);

    if attach {
        println!("Attaching to existing Vite server...");

        // Use RUNTIMED_VITE_PORT, fall back to CONDUCTOR_PORT, then default
        let port = env::var("RUNTIMED_VITE_PORT")
            .or_else(|_| env::var("CONDUCTOR_PORT"))
            .unwrap_or_else(|_| "5174".to_string());
        println!("Connecting to Vite at http://localhost:{port}");

        // Skip beforeDevCommand (Vite is already running) and set devUrl
        let config =
            format!(r#"{{"build":{{"devUrl":"http://localhost:{port}","beforeDevCommand":""}}}}"#);

        let mut args = vec!["tauri", "dev", "--config", &config, "--", "-p", "notebook"];
        if let Some(path) = notebook {
            args.extend(["--", path]);
        }

        run_cmd_with_rust_log("cargo", &args);
    } else {
        println!("Starting dev server with hot reload...");

        // Check for port override: RUNTIMED_VITE_PORT > CONDUCTOR_PORT
        let config_override = env::var("RUNTIMED_VITE_PORT")
            .map(|port| ("RUNTIMED_VITE_PORT", port))
            .or_else(|_| env::var("CONDUCTOR_PORT").map(|port| ("CONDUCTOR_PORT", port)))
            .ok()
            .map(|(var, port)| {
                println!("Using {var}={port}");
                format!(r#"{{"build":{{"devUrl":"http://localhost:{port}"}}}}"#)
            });

        let mut args = vec!["tauri", "dev"];
        if let Some(ref config) = config_override {
            args.extend(["--config", config]);
        }
        args.extend(["--", "-p", "notebook"]);
        if let Some(path) = notebook {
            args.extend(["--", path]);
        }

        run_cmd_with_rust_log("cargo", &args);
    }
}

fn cmd_vite() {
    println!("Starting Vite dev server...");
    println!("This server will keep running independently of Tauri.");
    println!("Use `cargo xtask dev --attach` in another terminal to connect.");
    println!();

    // Check for port override: RUNTIMED_VITE_PORT > CONDUCTOR_PORT
    if let Ok(port) = env::var("RUNTIMED_VITE_PORT") {
        println!("Using RUNTIMED_VITE_PORT={port}");
    } else if let Ok(port) = env::var("CONDUCTOR_PORT") {
        println!("Using CONDUCTOR_PORT={port}");
    }

    // Run pnpm dev for the notebook app
    run_cmd("pnpm", &["--filter", "notebook", "dev"]);
}

fn cmd_build(rust_only: bool) {
    // Build runtimed daemon binary for bundling (debug mode for faster builds)
    build_runtimed_daemon(false);

    if rust_only {
        // Check that frontend dist exists
        let dist_dir = Path::new("apps/notebook/dist");
        if !dist_dir.exists() {
            eprintln!("Error: No frontend build found at apps/notebook/dist");
            eprintln!("Run `cargo xtask build` (without --rust-only) first.");
            exit(1);
        }
        println!("Skipping frontend build (--rust-only), reusing existing assets");
    } else {
        // pnpm build runs: isolated-renderer + sidecar + notebook
        println!("Building frontend (isolated-renderer, sidecar, notebook)...");
        run_cmd("pnpm", &["build"]);
    }

    println!("Building debug binary (no bundle)...");
    run_cmd(
        "cargo",
        &[
            "tauri",
            "build",
            "--debug",
            "--no-bundle",
            "--config",
            r#"{"build":{"beforeBuildCommand":""}}"#,
        ],
    );

    // Write marker file to indicate this is a bundled build
    let marker = Path::new("./target/debug/.notebook-bundled");
    fs::write(marker, "bundled").unwrap_or_else(|e| {
        eprintln!("Warning: Could not write bundled marker: {e}");
    });

    println!("Build complete: ./target/debug/notebook");
}

fn cmd_run(notebook: Option<&str>) {
    let binary = Path::new("./target/debug/notebook");
    let marker = Path::new("./target/debug/.notebook-bundled");

    if !binary.exists() {
        eprintln!("Error: No binary found at ./target/debug/notebook");
        eprintln!("Run `cargo xtask build` first.");
        exit(1);
    }

    if !marker.exists() {
        eprintln!("Error: Binary appears to be a dev build (expects Vite server).");
        eprintln!("Run `cargo xtask build` for a standalone bundled binary.");
        exit(1);
    }

    println!("Running notebook app...");
    match notebook {
        Some(path) => run_cmd("./target/debug/notebook", &[path]),
        None => run_cmd("./target/debug/notebook", &[]),
    }
}

fn cmd_build_e2e() {
    // Build runtimed daemon binary for bundling (debug mode for faster builds)
    build_runtimed_daemon(false);

    // pnpm build runs: isolated-renderer + sidecar + notebook
    println!("Building frontend (isolated-renderer, sidecar, notebook)...");
    run_cmd("pnpm", &["build"]);

    println!("Building debug binary with WebDriver server...");
    run_cmd(
        "cargo",
        &[
            "tauri",
            "build",
            "--debug",
            "--no-bundle",
            "--features",
            "webdriver-test",
            "--config",
            r#"{"build":{"beforeBuildCommand":""}}"#,
        ],
    );

    println!("Build complete: ./target/debug/notebook");
    println!("Run with: ./target/debug/notebook --webdriver-port 4444");
}

fn cmd_icons(source: Option<&str>) {
    let default_source = "crates/notebook/icons/source.png";
    let source_path = source.unwrap_or(default_source);

    if !Path::new(source_path).exists() {
        eprintln!("Source icon not found: {source_path}");
        eprintln!("Export your icon from Figma to this location.");
        exit(1);
    }

    let output_dir = "crates/notebook/icons";

    println!("Generating icons from {source_path}...");
    run_cmd(
        "cargo",
        &["tauri", "icon", source_path, "--output", output_dir],
    );
    println!("Icons generated in {output_dir}/");
}

fn cmd_build_dmg() {
    build_with_bundle("dmg");
}

fn cmd_build_app() {
    build_with_bundle("app");
}

fn build_with_bundle(bundle: &str) {
    // Generate icons if source exists
    let source_path = "crates/notebook/icons/source.png";
    if Path::new(source_path).exists() {
        cmd_icons(None);
    } else {
        println!("Skipping icon generation (no source.png found)");
    }

    // Build runtimed daemon binary for bundling (release mode for distribution)
    build_runtimed_daemon(true);

    // Build frontend
    println!("Building frontend...");
    run_cmd("pnpm", &["build"]);

    // Build Tauri app
    println!("Building Tauri app ({bundle} bundle)...");
    run_cmd(
        "cargo",
        &[
            "tauri",
            "build",
            "--bundles",
            bundle,
            "--config",
            r#"{"build":{"beforeBuildCommand":""}}"#,
        ],
    );

    println!("Build complete!");
}

/// Build runtimed and install it into the running launchd/systemd service.
///
/// This is the dev workflow for testing daemon changes:
/// 1. Build runtimed in release mode
/// 2. Stop the running service
/// 3. Copy the new binary over the installed one
/// 4. Restart the service
fn cmd_install_daemon() {
    println!("Building runtimed (release)...");
    run_cmd("cargo", &["build", "--release", "-p", "runtimed"]);

    let source = if cfg!(windows) {
        "target/release/runtimed.exe"
    } else {
        "target/release/runtimed"
    };

    if !Path::new(source).exists() {
        eprintln!("Build succeeded but binary not found at {source}");
        exit(1);
    }

    // Use runtimed's own service manager to perform the upgrade.
    // The `runtimed install` CLI already handles stop → copy → chmod → start.
    // We call `runtimed upgrade --from <source>` if available, otherwise
    // fall back to the manual stop/copy/start dance.
    println!("Installing daemon...");

    // Stop the running daemon gracefully
    #[cfg(target_os = "macos")]
    {
        let uid = Command::new("id")
            .args(["-u"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "501".to_string());
        let domain = format!("gui/{uid}/io.nteract.runtimed");

        // Stop (ignore errors — may not be running)
        let _ = Command::new("launchctl")
            .args(["bootout", &domain])
            .status();

        // Brief pause for process cleanup
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", "runtimed.service"])
            .status();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Determine install path (matches runtimed::service::default_binary_path)
    let install_dir = dirs::data_local_dir()
        .expect("Could not determine data directory")
        .join("runt")
        .join("bin");

    let install_path = if cfg!(windows) {
        install_dir.join("runtimed.exe")
    } else {
        install_dir.join("runtimed")
    };

    if !install_path.exists() {
        eprintln!(
            "No existing daemon installation found at {}",
            install_path.display()
        );
        eprintln!("Run the app once first to install the daemon service.");
        exit(1);
    }

    // Copy new binary
    fs::copy(source, &install_path).unwrap_or_else(|e| {
        eprintln!("Failed to copy binary: {e}");
        exit(1);
    });

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&install_path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|e| {
            eprintln!("Failed to set permissions: {e}");
            exit(1);
        });
    }

    println!("Installed to {}", install_path.display());

    // Restart the service
    #[cfg(target_os = "macos")]
    {
        let plist = dirs::home_dir()
            .expect("No home dir")
            .join("Library/LaunchAgents/io.nteract.runtimed.plist");
        if plist.exists() {
            let uid = Command::new("id")
                .args(["-u"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "501".to_string());
            let domain = format!("gui/{uid}");
            run_cmd(
                "launchctl",
                &["bootstrap", &domain, &plist.to_string_lossy()],
            );
        } else {
            eprintln!("Warning: launchd plist not found at {}", plist.display());
            eprintln!("Start manually with: {}", install_path.display());
        }
    }

    #[cfg(target_os = "linux")]
    {
        run_cmd("systemctl", &["--user", "start", "runtimed.service"]);
    }

    // Wait briefly and verify
    std::thread::sleep(std::time::Duration::from_secs(2));
    let daemon_json = dirs::cache_dir()
        .unwrap_or_else(|| Path::new("/tmp").to_path_buf())
        .join("runt")
        .join("daemon.json");

    if daemon_json.exists() {
        if let Ok(contents) = fs::read_to_string(&daemon_json) {
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
                    println!("Daemon running: version {version}");
                    return;
                }
            }
        }
    }

    println!("Daemon restarted (could not verify version from daemon.json)");
}

/// Build and run runtimed in per-worktree development mode.
///
/// This enables isolated daemon instances per git worktree, useful when
/// developing/testing daemon code across multiple worktrees simultaneously.
fn cmd_dev_daemon() {
    println!("Building runtimed (debug)...");
    run_cmd("cargo", &["build", "-p", "runtimed"]);

    let binary = if cfg!(windows) {
        "target/debug/runtimed.exe"
    } else {
        "target/debug/runtimed"
    };

    if !Path::new(binary).exists() {
        eprintln!("Build succeeded but binary not found at {binary}");
        exit(1);
    }

    let cache_base = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("runt")
        .join("worktrees");

    let state_dir = match runt_workspace::get_workspace_path() {
        Some(path) => cache_base.join(runt_workspace::worktree_hash(&path)),
        None => cache_base.join("<unknown>"),
    };

    println!();
    println!("Starting development daemon for this worktree...");
    println!("State will be stored in {}/", state_dir.display());
    println!("Press Ctrl+C to stop.");
    println!();

    // Run the daemon with --dev flag
    let mut cmd = Command::new(binary);
    cmd.args(["--dev", "run"]);
    cmd.env("RUNTIMED_DEV", "1");

    // Translate Conductor → Runtimed for Conductor workspace users
    if let Ok(path) = env::var("CONDUCTOR_WORKSPACE_PATH") {
        cmd.env("RUNTIMED_WORKSPACE_PATH", &path);
    }
    if let Ok(name) = env::var("CONDUCTOR_WORKSPACE_NAME") {
        cmd.env("RUNTIMED_WORKSPACE_NAME", &name);
    }

    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("Failed to run runtimed: {e}");
        exit(1);
    });

    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
}

/// Build external binaries (runtimed daemon and runt CLI) for Tauri bundling.
/// If `release` is true, builds in release mode (for distribution).
/// If `release` is false, builds in debug mode (faster for development).
fn build_runtimed_daemon(release: bool) {
    build_external_binary("runtimed", "runtimed", release);
    ensure_sidecar_ui();
    build_external_binary("runt-cli", "runt", release);
}

/// Ensure sidecar UI assets exist (required before building runt-cli).
fn ensure_sidecar_ui() {
    let sidecar_dist = Path::new("apps/sidecar/dist/index.html");
    if !sidecar_dist.exists() {
        println!("Building sidecar UI (required for runt-cli)...");
        run_cmd("pnpm", &["--dir", "apps/sidecar", "build"]);
    }
}

/// Build a binary and copy to binaries/ with target triple suffix for Tauri bundling.
/// If `release` is true, builds in release mode. Otherwise builds in debug mode.
fn build_external_binary(package: &str, binary_name: &str, release: bool) {
    let mode = if release { "release" } else { "debug" };
    println!("Building {binary_name} ({mode})...");

    // Get the host target triple
    let target = get_host_target();

    // Build with appropriate profile
    if release {
        run_cmd("cargo", &["build", "--release", "-p", package]);
    } else {
        run_cmd("cargo", &["build", "-p", package]);
    }

    // Determine source and destination paths
    let target_dir = if release {
        "target/release"
    } else {
        "target/debug"
    };
    let source = if cfg!(windows) {
        format!("{target_dir}/{binary_name}.exe")
    } else {
        format!("{target_dir}/{binary_name}")
    };

    let dest_name = if cfg!(windows) {
        format!("{binary_name}-{target}.exe")
    } else {
        format!("{binary_name}-{target}")
    };

    // Copy to crates/notebook/binaries/ for Tauri bundle builds
    let binaries_dir = Path::new("crates/notebook/binaries");
    let dest = binaries_dir.join(&dest_name);
    fs::copy(&source, &dest).unwrap_or_else(|e| {
        eprintln!("Failed to copy {binary_name} binary: {e}");
        exit(1);
    });
    println!("{binary_name} ready: {}", dest.display());

    // Also copy to target/debug/binaries/ for development (no-bundle builds)
    // Tauri's externalBin only copies to app bundle, not for --no-bundle
    let dev_binaries_dir = Path::new("target/debug/binaries");
    fs::create_dir_all(dev_binaries_dir).ok();
    let dev_dest = dev_binaries_dir.join(&dest_name);
    fs::copy(&source, &dev_dest).unwrap_or_else(|e| {
        eprintln!("Failed to copy {binary_name} to dev binaries: {e}");
        exit(1);
    });
    println!("{binary_name} dev ready: {}", dev_dest.display());
}

/// Get the host target triple (e.g., aarch64-apple-darwin).
fn get_host_target() -> String {
    let output = Command::new("rustc")
        .args(["--print", "host-tuple"])
        .output()
        .expect("Failed to get host target from rustc");

    String::from_utf8(output.stdout)
        .expect("Invalid UTF-8 from rustc")
        .trim()
        .to_string()
}

fn run_cmd(cmd: &str, args: &[&str]) {
    let status = Command::new(cmd).args(args).status().unwrap_or_else(|e| {
        eprintln!("Failed to run {cmd}: {e}");
        exit(1);
    });

    if !status.success() {
        eprintln!("Command failed: {cmd} {}", args.join(" "));
        exit(status.code().unwrap_or(1));
    }
}

/// Run a command with RUST_LOG set to enable info-level logging.
/// This is useful for dev mode to see Rust logs from the notebook app.
/// Also translates CONDUCTOR_* env vars to RUNTIMED_* for Conductor workspace users.
fn run_cmd_with_rust_log(cmd: &str, args: &[&str]) {
    // Use existing RUST_LOG if set, otherwise default to info
    let rust_log = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let mut command = Command::new(cmd);
    command.args(args).env("RUST_LOG", &rust_log);

    // Translate Conductor → Runtimed for Conductor workspace users
    if let Ok(path) = env::var("CONDUCTOR_WORKSPACE_PATH") {
        command.env("RUNTIMED_WORKSPACE_PATH", &path);
    }
    if let Ok(name) = env::var("CONDUCTOR_WORKSPACE_NAME") {
        command.env("RUNTIMED_WORKSPACE_NAME", &name);
    }

    let status = command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run {cmd}: {e}");
        exit(1);
    });

    if !status.success() {
        eprintln!("Command failed: {cmd} {}", args.join(" "));
        exit(status.code().unwrap_or(1));
    }
}
