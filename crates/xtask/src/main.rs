use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{exit, Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        exit(0);
    }

    match args[0].as_str() {
        "dev" => {
            let options = parse_dev_options(&args);
            cmd_dev(options.notebook, options.skip_install, options.skip_build);
        }
        "notebook" => {
            let attach = args.iter().any(|a| a == "--attach");
            let notebook = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .map(String::as_str);
            cmd_notebook(notebook, attach);
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
        "dev-daemon" => {
            let release = args.iter().any(|a| a == "--release");
            cmd_dev_daemon(release);
        }
        "lint" => {
            let fix = args.iter().any(|a| a == "--fix");
            cmd_lint(fix);
        }
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
  dev [notebook.ipynb]         Setup once, start dev daemon + notebook app
  dev --skip-build             Reuse existing build artifacts before launch
  dev --skip-install           Reuse existing pnpm install before launch
  notebook [notebook.ipynb]    Start hot-reload dev server (Vite + Tauri)
  notebook --attach [notebook] Attach Tauri to existing Vite server
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
  dev-daemon [--release]     Build and run runtimed in per-worktree dev mode

Linting:
  lint                       Check formatting and linting (Rust, JS/TS, Python)
  lint --fix                 Auto-fix formatting and linting issues

Other:
  icons [source.png]         Generate icon variants
  help                       Show this help
"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DevOptions<'a> {
    notebook: Option<&'a str>,
    skip_install: bool,
    skip_build: bool,
}

fn parse_dev_options(args: &[String]) -> DevOptions<'_> {
    DevOptions {
        notebook: args
            .iter()
            .skip(1)
            .find(|arg| !arg.starts_with('-'))
            .map(String::as_str),
        skip_install: args.iter().any(|arg| arg == "--skip-install"),
        skip_build: args.iter().any(|arg| arg == "--skip-build"),
    }
}

fn cmd_dev(notebook: Option<&str>, skip_install: bool, skip_build: bool) {
    if skip_install {
        println!("Skipping pnpm install (--skip-install)");
    } else {
        ensure_pnpm_install();
    }

    if skip_build {
        println!("Skipping cargo xtask build (--skip-build)");
        ensure_dev_daemon_binaries();
    } else {
        println!("Running cargo xtask build for first-time setup...");
        cmd_build(false);
    }

    println!();
    let mut daemon = None;
    if dev_daemon_running() {
        println!("Reusing existing development daemon for this worktree.");
    } else {
        println!("Starting development daemon for one-shot notebook workflow...");
        let mut child = spawn_dev_daemon_process(false);
        if let Err(error) = wait_for_dev_daemon(&mut child, Duration::from_secs(30)) {
            stop_child(&mut child, "development daemon");
            eprintln!("{error}");
            exit(1);
        }
        println!("Development daemon is ready.");
        daemon = Some(child);
    }
    println!();

    let status = run_notebook_dev_app(notebook, false, true);
    if let Some(ref mut child) = daemon {
        stop_child(child, "development daemon");
    }
    exit_on_failed_status("cargo tauri dev", status);
}

fn cmd_notebook(notebook: Option<&str>, attach: bool) {
    ensure_pnpm_install();
    let status = run_notebook_dev_app(notebook, attach, false);
    exit_on_failed_status("cargo tauri dev", status);
}

fn run_notebook_dev_app(notebook: Option<&str>, attach: bool, force_dev_mode: bool) -> ExitStatus {
    // Delete bundled marker since we're building a dev binary
    let marker = Path::new("./target/debug/.notebook-bundled");
    let _ = fs::remove_file(marker);

    let vite_port = resolve_vite_port(force_dev_mode);
    let mut command = Command::new("cargo");

    if attach {
        println!("Attaching to existing Vite server...");
        let port = vite_port.clone().unwrap_or_else(|| "5174".to_string());
        println!("Connecting to Vite at http://localhost:{port}");

        // Skip beforeDevCommand (Vite is already running), set devUrl,
        // and drop externalBin so sidecar binaries aren't required in dev
        let config = format!(
            r#"{{"build":{{"devUrl":"http://localhost:{port}","beforeDevCommand":""}},"bundle":{{"externalBin":[]}}}}"#
        );

        let mut args = vec!["tauri", "dev", "--config", &config, "--", "-p", "notebook"];
        if let Some(path) = notebook {
            args.extend(["--", path]);
        }

        command.args(&args);
    } else {
        println!("Starting dev server with hot reload...");

        // Always override externalBin so sidecar binaries aren't required
        // in dev mode (the daemon is started separately via dev-daemon)
        let config_override = match vite_port.as_ref() {
            Some(port) => {
                println!("Using RUNTIMED_VITE_PORT={port}");
                format!(
                    r#"{{"build":{{"devUrl":"http://localhost:{port}"}},"bundle":{{"externalBin":[]}}}}"#
                )
            }
            None => r#"{"bundle":{"externalBin":[]}}"#.to_string(),
        };

        let mut args = vec!["tauri", "dev", "--config", &config_override];
        args.extend(["--", "-p", "notebook"]);
        if let Some(path) = notebook {
            args.extend(["--", path]);
        }

        command.args(&args);
    }

    apply_rust_log_env(&mut command);
    apply_build_channel_env(&mut command);
    apply_worktree_env(&mut command, force_dev_mode);
    if let Some(ref port) = vite_port {
        command.env("RUNTIMED_VITE_PORT", port);
    }

    command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run cargo tauri dev: {e}");
        exit(1);
    })
}

fn cmd_vite() {
    println!("Starting Vite dev server...");
    println!("This server will keep running independently of Tauri.");
    println!("Use `cargo xtask notebook --attach` in another terminal to connect.");
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

fn ensure_pnpm_install() {
    if let Some(reason) = pnpm_install_reason() {
        println!("Running pnpm install ({reason})...");
        run_cmd("pnpm", &["install"]);
    } else {
        println!("Skipping pnpm install (node_modules is up to date).");
    }
}

fn pnpm_install_reason() -> Option<&'static str> {
    let install_marker = Path::new("node_modules/.modules.yaml");
    if !install_marker.exists() {
        return Some("missing node_modules metadata");
    }

    let Some(install_time) = modified_time(install_marker) else {
        return Some("could not read node_modules metadata timestamp");
    };
    for manifest in [Path::new("package.json"), Path::new("pnpm-lock.yaml")] {
        let Some(manifest_time) = modified_time(manifest) else {
            return Some("could not read package manifest timestamps");
        };
        if manifest_time > install_time {
            return Some("package manifests changed");
        }
    }

    None
}

fn modified_time(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
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
        // pnpm build runs: notebook UI
        println!("Building frontend (notebook)...");
        run_frontend_build(true);
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

    // pnpm build runs: notebook UI
    println!("Building frontend (notebook)...");
    run_frontend_build(true);

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
    run_frontend_build(false);

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
#[allow(clippy::expect_used)] // xtask is a dev tool; panics with context are fine here
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
        let domain = format!("gui/{uid}/{}", runt_workspace::daemon_launchd_label());

        // Stop (ignore errors — may not be running)
        let _ = Command::new("launchctl")
            .args(["bootout", &domain])
            .status();

        // Brief pause for process cleanup
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    #[cfg(target_os = "linux")]
    {
        let service = format!("{}.service", runt_workspace::daemon_service_basename());
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &service])
            .status();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Determine install path (matches runtimed::service::default_binary_path)
    let install_dir = dirs::data_local_dir()
        .expect("Could not determine data directory")
        .join(runt_workspace::cache_namespace())
        .join("bin");

    let binary_name = runt_workspace::daemon_binary_basename();
    let install_path = if cfg!(windows) {
        install_dir.join(format!("{binary_name}.exe"))
    } else {
        install_dir.join(binary_name)
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
        let plist = dirs::home_dir().expect("No home dir").join(format!(
            "Library/LaunchAgents/{}.plist",
            runt_workspace::daemon_launchd_label()
        ));
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
        let service = format!("{}.service", runt_workspace::daemon_service_basename());
        run_cmd("systemctl", &["--user", "start", &service]);
    }

    // Wait briefly and verify
    std::thread::sleep(std::time::Duration::from_secs(2));
    let daemon_json = dirs::cache_dir()
        .unwrap_or_else(|| Path::new("/tmp").to_path_buf())
        .join(runt_workspace::cache_namespace())
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
fn cmd_dev_daemon(release: bool) {
    if release {
        println!("Building runtimed (release)...");
        run_cmd("cargo", &["build", "--release", "-p", "runtimed"]);
    } else {
        println!("Building runtimed (debug)...");
        run_cmd("cargo", &["build", "-p", "runtimed"]);
    }

    let binary = if cfg!(windows) {
        if release {
            "target/release/runtimed.exe"
        } else {
            "target/debug/runtimed.exe"
        }
    } else if release {
        "target/release/runtimed"
    } else {
        "target/debug/runtimed"
    };

    if !Path::new(binary).exists() {
        eprintln!("Build succeeded but binary not found at {binary}");
        exit(1);
    }

    let cache_base = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(runt_workspace::cache_namespace())
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

fn ensure_dev_daemon_binaries() {
    println!("Building runtimed + runt binaries for dev daemon...");
    build_runtimed_daemon(false);
}

fn spawn_dev_daemon_process(release: bool) -> Child {
    ensure_dev_daemon_binaries();

    let binary = dev_daemon_binary(release);
    let cache_base = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(runt_workspace::cache_namespace())
        .join("worktrees");

    let state_dir = match runt_workspace::get_workspace_path() {
        Some(path) => cache_base.join(runt_workspace::worktree_hash(&path)),
        None => cache_base.join("<unknown>"),
    };

    println!("State will be stored in {}/", state_dir.display());
    println!("Notebook command will stop the daemon when the app exits.");
    println!();

    let mut command = Command::new(binary);
    command
        .args(["--dev", "run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_worktree_env(&mut command, true);

    let mut child = command.spawn().unwrap_or_else(|e| {
        eprintln!("Failed to run runtimed: {e}");
        exit(1);
    });

    relay_child_output("daemon", child.stdout.take());
    relay_child_output("daemon", child.stderr.take());
    child
}

fn wait_for_dev_daemon(child: &mut Child, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Failed to poll dev daemon status: {error}"))?
        {
            return Err(format!(
                "Development daemon exited before it became ready (status: {status})."
            ));
        }

        if dev_daemon_running() {
            return Ok(());
        }

        thread::sleep(Duration::from_millis(250));
    }

    Err("Timed out waiting for the development daemon to become ready.".to_string())
}

fn dev_daemon_running() -> bool {
    let mut command = Command::new(dev_runt_cli_binary());
    command
        .args(["daemon", "status", "--json"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    apply_worktree_env(&mut command, true);

    let output = command.output();

    let output = match output {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    let status_json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(json) => json,
        Err(_) => return false,
    };

    status_json
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || fallback_dev_daemon_running()
}

fn fallback_dev_daemon_running() -> bool {
    let Some(workspace) = runt_workspace::get_workspace_path() else {
        return false;
    };

    let daemon_json = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(runt_workspace::cache_namespace())
        .join("worktrees")
        .join(runt_workspace::worktree_hash(&workspace))
        .join("daemon.json");

    daemon_state_is_running(&daemon_json)
}

fn daemon_state_is_running(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(info) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };

    let pid_running = info
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(process_is_running)
        .unwrap_or(false);
    if pid_running {
        return true;
    }

    info.get("endpoint")
        .and_then(serde_json::Value::as_str)
        .map(Path::new)
        .is_some_and(Path::exists)
}

fn process_is_running(pid: u64) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn dev_daemon_binary(release: bool) -> &'static str {
    if cfg!(windows) {
        if release {
            "target/release/runtimed.exe"
        } else {
            "target/debug/runtimed.exe"
        }
    } else if release {
        "target/release/runtimed"
    } else {
        "target/debug/runtimed"
    }
}

fn dev_runt_cli_binary() -> &'static str {
    if cfg!(windows) {
        "target/debug/runt.exe"
    } else {
        "target/debug/runt"
    }
}

fn relay_child_output<R>(label: &'static str, stream: Option<R>)
where
    R: std::io::Read + Send + 'static,
{
    let Some(stream) = stream else {
        return;
    };

    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(line) => eprintln!("[{label}] {line}"),
                Err(_) => break,
            }
        }
    });
}

fn stop_child(child: &mut Child, label: &str) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            println!("Stopping {label}...");
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(error) => {
            eprintln!("Failed to poll {label}: {error}");
        }
    }
}

fn resolve_vite_port(force_dev_mode: bool) -> Option<String> {
    env::var("RUNTIMED_VITE_PORT")
        .ok()
        .or_else(|| env::var("CONDUCTOR_PORT").ok())
        .or_else(|| {
            if force_dev_mode {
                default_dev_vite_port().map(|port| port.to_string())
            } else {
                None
            }
        })
}

fn default_dev_vite_port() -> Option<u16> {
    let workspace = runt_workspace::get_workspace_path()?;
    Some(default_vite_port_for_workspace(&workspace))
}

fn default_vite_port_for_workspace(path: &Path) -> u16 {
    let hash = runt_workspace::worktree_hash(path);
    let prefix = hash.get(..4).unwrap_or("0000");
    let offset = u16::from_str_radix(prefix, 16).unwrap_or(0) % 4900;
    5100 + offset
}

/// Run linting and formatting checks across all languages.
///
/// In check mode (default): exits non-zero if any issues are found.
/// In fix mode (--fix): auto-fixes issues where possible.
fn cmd_lint(fix: bool) {
    let mode = if fix { "fix" } else { "check" };
    println!("Running lint ({mode} mode)...");
    println!();

    // Track if any linter failed
    let mut failed = false;

    // Rust formatting
    println!("=== Rust formatting ===");
    if fix {
        if !run_cmd_ok("cargo", &["fmt"]) {
            failed = true;
        }
    } else if !run_cmd_ok("cargo", &["fmt", "--check"]) {
        failed = true;
    }
    println!();

    // Rust clippy (check-only, no auto-fix available)
    if !fix {
        println!("=== Rust clippy ===");
        if !run_cmd_ok(
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ) {
            failed = true;
        }
        println!();
    }

    // JavaScript/TypeScript with Biome
    println!("=== JavaScript/TypeScript (Biome) ===");
    let biome_ok = if fix {
        run_cmd_ok(
            "npx",
            &[
                "@biomejs/biome",
                "check",
                "--fix",
                "apps/notebook/src/",
                "e2e/",
            ],
        )
    } else {
        run_cmd_ok(
            "npx",
            &["@biomejs/biome", "check", "apps/notebook/src/", "e2e/"],
        )
    };
    if !biome_ok {
        failed = true;
    }
    println!();

    // Python with ruff (if uv is available)
    let python_dir = Path::new("python");
    if python_dir.exists() {
        if Command::new("uv").arg("--version").output().is_ok() {
            println!("=== Python (ruff) ===");

            // ruff check
            let check_args = if fix {
                vec!["run", "ruff", "check", "--fix", "."]
            } else {
                vec!["run", "ruff", "check", "."]
            };
            let check_status = Command::new("uv")
                .args(&check_args)
                .current_dir(python_dir)
                .status();
            if !check_status.map(|s| s.success()).unwrap_or(false) {
                failed = true;
            }

            // ruff format
            let format_args = if fix {
                vec!["run", "ruff", "format", "."]
            } else {
                vec!["run", "ruff", "format", "--check", "."]
            };
            let format_status = Command::new("uv")
                .args(&format_args)
                .current_dir(python_dir)
                .status();
            if !format_status.map(|s| s.success()).unwrap_or(false) {
                failed = true;
            }
            println!();
        } else {
            println!("=== Python (ruff) ===");
            println!("Skipping: uv not found in PATH");
            println!();
        }
    }

    if failed {
        if fix {
            eprintln!("Some issues could not be auto-fixed. See output above.");
        } else {
            eprintln!("Lint check failed. Run `cargo xtask lint --fix` to auto-fix.");
        }
        exit(1);
    }

    println!("All checks passed!");
}

/// Run a command and return true if it succeeded.
fn run_cmd_ok(cmd: &str, args: &[&str]) -> bool {
    let mut command = Command::new(cmd);
    command.args(args);
    if cmd == "cargo" {
        apply_build_channel_env(&mut command);
    }

    command.status().map(|s| s.success()).unwrap_or_else(|e| {
        eprintln!("Failed to run {cmd}: {e}");
        false
    })
}

/// Build external binaries (runtimed daemon and runt CLI) for Tauri bundling.
/// If `release` is true, builds in release mode (for distribution).
/// If `release` is false, builds in debug mode (faster for development).
fn build_runtimed_daemon(release: bool) {
    build_external_binary("runtimed", "runtimed", release);
    build_external_binary("runt-cli", "runt", release);
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
#[allow(clippy::expect_used)] // xtask is a dev tool; rustc must be available
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
    let mut command = Command::new(cmd);
    command.args(args);
    if cmd == "cargo" {
        apply_build_channel_env(&mut command);
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

fn run_frontend_build(debug_bundle: bool) {
    let mut command = Command::new("pnpm");
    command.arg("build");
    if debug_bundle {
        command.env("RUNT_NOTEBOOK_DEBUG_BUILD", "1");
    }

    let status = command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run pnpm build: {e}");
        exit(1);
    });

    if !status.success() {
        eprintln!("Command failed: pnpm build");
        exit(status.code().unwrap_or(1));
    }
}

fn apply_rust_log_env(command: &mut Command) {
    let rust_log = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    command.env("RUST_LOG", rust_log);
}

fn apply_build_channel_env(command: &mut Command) {
    let build_channel = env::var("RUNT_BUILD_CHANNEL")
        .unwrap_or_else(|_| runt_workspace::channel_display_name().to_string());
    command.env("RUNT_BUILD_CHANNEL", build_channel);
}

fn apply_worktree_env(command: &mut Command, force_dev_mode: bool) {
    if force_dev_mode {
        command.env("RUNTIMED_DEV", "1");
    }

    if let Ok(path) = env::var("CONDUCTOR_WORKSPACE_PATH") {
        command.env("RUNTIMED_WORKSPACE_PATH", path);
    } else if force_dev_mode {
        if let Some(path) = runt_workspace::get_workspace_path() {
            command.env("RUNTIMED_WORKSPACE_PATH", path);
        }
    }

    if let Ok(name) = env::var("CONDUCTOR_WORKSPACE_NAME") {
        command.env("RUNTIMED_WORKSPACE_NAME", name);
    } else if force_dev_mode {
        if let Some(name) = runt_workspace::get_workspace_name() {
            command.env("RUNTIMED_WORKSPACE_NAME", name);
        }
    }
}

fn exit_on_failed_status(label: &str, status: ExitStatus) {
    if !status.success() {
        eprintln!("{label} exited with status {status}");
        exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_dev_options_reads_flags_and_path() {
        let args = vec![
            "dev".to_string(),
            "--skip-install".to_string(),
            "notebooks/demo.ipynb".to_string(),
            "--skip-build".to_string(),
        ];

        let options = parse_dev_options(&args);
        assert_eq!(
            options,
            DevOptions {
                notebook: Some("notebooks/demo.ipynb"),
                skip_install: true,
                skip_build: true,
            }
        );
    }

    #[test]
    fn default_vite_port_is_stable_for_workspace() {
        let workspace = Path::new("/workspace/example");
        let port = default_vite_port_for_workspace(workspace);
        assert_eq!(port, default_vite_port_for_workspace(workspace));
        assert!((5100u16..10000u16).contains(&port));
    }
}
