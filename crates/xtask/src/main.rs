use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{exit, Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
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
        "e2e" => {
            let sub_args: Vec<String> = args[1..].to_vec();
            cmd_e2e(sub_args);
        }
        "build-e2e" => cmd_build_e2e(),
        "build-dmg" => cmd_build_dmg(),
        "build-app" => cmd_build_app(),
        "install-daemon" => cmd_install_daemon(),
        "dev-daemon" => {
            let release = args.iter().any(|a| a == "--release");
            cmd_dev_daemon(release);
        }
        "dev-mcp" => {
            let print_config = args.iter().any(|a| a == "--print-config");
            cmd_dev_mcp(print_config);
        }
        "run-mcp" | "mcp" => {
            let print_config = args.iter().any(|a| a == "--print-config");
            let release = args.iter().any(|a| a == "--release");
            cmd_mcp(print_config, release);
        }
        "lint" => {
            let fix = args.iter().any(|a| a == "--fix");
            cmd_lint(fix);
        }
        "integration" => {
            let filter = args.iter().find(|a| !a.starts_with('-')).cloned();
            cmd_integration(filter);
        }
        "wasm" => cmd_wasm(),
        "mcpb" => {
            let output = args
                .windows(2)
                .find(|w| w[0] == "--output")
                .map(|w| w[1].as_str());
            let variant = args
                .windows(2)
                .find(|w| w[0] == "--variant")
                .map(|w| w[1].as_str())
                .unwrap_or("stable");
            cmd_mcpb(output, variant);
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
  notebook [notebook.ipynb]    Start hot-reload dev server (dev mode, safe)
  notebook --attach [notebook] Attach Tauri to existing Vite server
  vite                       Start Vite server standalone
  build                      Full debug build (frontend + rust)
  build --rust-only          Rebuild rust only, reuse existing frontend
  run [notebook.ipynb]       Run bundled debug binary

Release:
  build-app                  Build .app bundle with icons
  build-dmg                  Build DMG with icons (for CI)

Daemon:
  install-daemon             Build and install runtimed into the running service
  dev-daemon [--release]     Build and run runtimed in per-worktree dev mode

MCP:
  run-mcp [--release]        Build and run the nteract-dev MCP supervisor (proxy + daemon + auto-restart)
  run-mcp --print-config     Print MCP client config JSON (for Zed, Claude, etc.)
  dev-mcp                    Build Python bindings and launch nteract MCP server directly (no supervisor)
  dev-mcp --print-config     Print MCP client config JSON (for Zed, Claude, etc.)

Linting:
  lint                       Check formatting and linting (Rust, JS/TS, Python)
  lint --fix                 Auto-fix formatting and linting issues

Testing:
  integration [filter]       Run Python integration tests with an isolated daemon
                             Optional filter is passed to pytest -k (e.g. 'test_start_kernel')
  e2e [build|test|test-fixture|test-all]
                             E2E testing (build, run, manage fixtures)

Other:
  wasm                       Rebuild runtimed-wasm (wasm-pack build)
  icons [source.png]         Generate icon variants
  mcpb                       Package nteract as a Claude Desktop extension (.mcpb)
  mcpb --variant nightly     Build nightly variant (different name/icon)
  mcpb --output <path>       Write the .mcpb archive to a custom path
  help                       Show this help
"
    );
}

/// Run Python integration tests with a fresh isolated daemon.
///
/// Builds the daemon binary, spawns it in a temp directory with its own
/// worktree hash (no singleton conflicts), and runs pytest against it.
/// The daemon is cleaned up when tests finish.
fn cmd_integration(filter: Option<String>) {
    // 1. Build the daemon
    println!("Building runtimed for integration tests...");
    let status = Command::new("cargo")
        .args(["build", "-p", "runtimed"])
        .status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        eprintln!("Failed to build runtimed");
        exit(1);
    }

    // 2. Ensure Python env is ready
    ensure_python_env();
    ensure_maturin_develop();

    // 3. Create an isolated workspace path so the daemon gets its own
    //    worktree hash and doesn't conflict with the dev daemon.
    let workspace_dir =
        std::env::temp_dir().join(format!("runtimed-integration-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create temp workspace: {e}");
        exit(1);
    });

    // 4. Build pytest args
    let binary = std::fs::canonicalize("target/debug/runtimed").unwrap_or_else(|e| {
        eprintln!("Failed to resolve runtimed binary: {e}");
        exit(1);
    });

    let mut pytest_args = vec![
        "run".to_string(),
        "pytest".to_string(),
        "python/runtimed/tests/test_daemon_integration.py".to_string(),
        "-v".to_string(),
        "--timeout=120".to_string(),
        "--tb=short".to_string(),
        "--durations=15".to_string(),
    ];
    if let Some(ref f) = filter {
        pytest_args.push("-k".to_string());
        pytest_args.push(f.clone());
    }

    println!("Running integration tests...");
    println!("  Daemon binary: {}", binary.display());
    println!("  Workspace: {}", workspace_dir.display());
    if let Some(ref f) = filter {
        println!("  Filter: {f}");
    }
    println!();

    // 5. Run pytest with CI mode env vars
    let status = Command::new("uv")
        .args(&pytest_args)
        .env("RUNTIMED_INTEGRATION_TEST", "1")
        .env("RUNTIMED_BINARY", &binary)
        .env("RUNTIMED_WORKSPACE_PATH", &workspace_dir)
        .env("RUNTIMED_LOG_LEVEL", "info")
        .status();

    // 6. Cleanup temp workspace
    let _ = std::fs::remove_dir_all(&workspace_dir);

    match status {
        Ok(s) if s.success() => {
            println!("\nAll integration tests passed!");
        }
        Ok(s) => {
            eprintln!("\nSome integration tests failed.");
            exit(s.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("Failed to run pytest: {e}");
            exit(1);
        }
    }
}

/// Check that an external tool is available in PATH, exit with install instructions if not.
fn require_tool(name: &str, install_hint: &str) {
    let ok = Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("Error: `{name}` is required but was not found in PATH.");
        eprintln!();
        eprintln!("  Install:  {install_hint}");
        exit(1);
    }
}

/// Check that a cargo subcommand (e.g. `cargo tauri`) is available.
fn require_cargo_subcommand(name: &str, install_hint: &str) {
    let ok = Command::new("cargo")
        .args([name, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("Error: `cargo {name}` is required but was not found.");
        eprintln!();
        eprintln!("  Install:  {install_hint}");
        exit(1);
    }
}

const PNPM_INSTALL: &str = "brew install pnpm  (or: npm install -g pnpm)";
const TAURI_INSTALL: &str = "cargo install tauri-cli";
const WASM_PACK_INSTALL: &str = "cargo install wasm-pack";

fn require_pnpm() {
    require_tool("pnpm", PNPM_INSTALL);
}

fn require_tauri() {
    require_cargo_subcommand("tauri", TAURI_INSTALL);
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
    require_pnpm();
    require_tauri();

    if skip_install {
        println!("Skipping pnpm install (--skip-install)");
    } else {
        ensure_pnpm_install();
        ensure_python_env();
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
    require_pnpm();
    require_tauri();

    // Always use dev mode to prevent the Tauri app from auto-installing
    // the dev binary as the system daemon sidecar — that would clobber
    // any running nightly/release daemon and disconnect all open notebooks.
    //
    // In dev mode, ensure_daemon_via_sidecar() skips auto-install and
    // tells the user to run `cargo xtask dev-daemon` instead.
    if !dev_daemon_running() {
        eprintln!("⚠️  No dev daemon detected for this worktree.");
        eprintln!("   Start one first:  cargo xtask dev-daemon");
        eprintln!("   Or use the full workflow:  cargo xtask dev");
        eprintln!();
        eprintln!("   Running without a dev daemon will connect to the system daemon,");
        eprintln!("   which may disrupt other notebooks. Proceeding in dev mode anyway...");
        eprintln!();
    }
    ensure_pnpm_install();
    let status = run_notebook_dev_app(notebook, attach, true);
    exit_on_failed_status("cargo tauri dev", status);
}

fn run_notebook_dev_app(notebook: Option<&str>, attach: bool, force_dev_mode: bool) -> ExitStatus {
    // Delete bundled marker since we're building a dev binary
    let marker = Path::new("./target/debug/.notebook-bundled");
    let _ = fs::remove_file(marker);

    let vite_port = resolve_vite_port(force_dev_mode);
    let mut command = Command::new("cargo");
    apply_sccache_env(&mut command);

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
    require_pnpm();

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
    run_cmd("pnpm", &["--filter", "notebook-ui", "dev"]);
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

/// Ensure the Python workspace venv is synced (`uv sync`).
///
/// This installs all workspace members (nteract, runtimed) and their
/// dependencies (mcp, pydantic, etc.) into `.venv`. Needed for:
/// - `maturin develop` (installs into this venv)
/// - `uv run --no-sync` (expects deps to be present)
/// - Editor type-checking / LSP (needs the venv to resolve imports)
fn ensure_python_env() {
    if !Path::new("pyproject.toml").exists() {
        return;
    }
    if Command::new("uv").arg("--version").output().is_err() {
        println!("Skipping Python env sync (uv not found).");
        return;
    }

    if let Some(reason) = python_sync_reason() {
        println!("Syncing Python workspace ({reason})...");
        let status = Command::new("uv").args(["sync"]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("Warning: uv sync failed (exit {})", s.code().unwrap_or(-1));
            }
            Err(e) => {
                eprintln!("Warning: failed to run uv sync: {e}");
            }
        }
    } else {
        println!("Skipping Python env sync (venv is up to date).");
    }
}

fn python_sync_reason() -> Option<&'static str> {
    let venv_marker = Path::new(".venv/pyvenv.cfg");
    if !venv_marker.exists() {
        return Some("missing .venv");
    }

    let Some(venv_time) = modified_time(venv_marker) else {
        return Some("could not read .venv timestamp");
    };

    for manifest in [
        Path::new("uv.lock"),
        Path::new("pyproject.toml"),
        Path::new("python/nteract/pyproject.toml"),
        Path::new("python/runtimed/pyproject.toml"),
    ] {
        if let Some(manifest_time) = modified_time(manifest) {
            if manifest_time > venv_time {
                return Some("pyproject.toml or uv.lock changed");
            }
        }
    }

    None
}

/// Ensure `maturin develop` has been run so the native `runtimed` extension
/// is installed into `.venv`.
///
/// Unlike `uv sync` (which builds a release wheel), `maturin develop` builds
/// a debug `.so` and symlinks it — faster to compile and always reflects the
/// latest Rust source.
fn ensure_maturin_develop() {
    if !Path::new("pyproject.toml").exists() {
        return;
    }
    if Command::new("uv").arg("--version").output().is_err() {
        println!("Skipping maturin develop (uv not found).");
        return;
    }

    println!("Building runtimed Python bindings (maturin develop)...");
    // Resolve absolute path — maturin warns on relative VIRTUAL_ENV.
    // cargo xtask always runs from the workspace root (all paths in this
    // file are relative to it), so current_dir() is the repo root.
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("Warning: failed to get current directory for maturin develop");
        return;
    };
    let status = Command::new("uv")
        .args([
            "run",
            "--directory",
            "python/runtimed",
            "maturin",
            "develop",
        ])
        .env("VIRTUAL_ENV", cwd.join(".venv"))
        .env_remove("CONDA_PREFIX")
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "Warning: maturin develop failed (exit {})",
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("Warning: failed to run maturin develop: {e}");
        }
    }
}

fn cmd_build(rust_only: bool) {
    require_tauri();
    if !rust_only {
        require_pnpm();
    }

    // Phase 0: Build the MCP widget HTML before any Rust compilation.
    // runt-mcp uses include_str!("../assets/_output.html") which fails
    // if the asset doesn't exist yet. This must run before cargo build.
    if !rust_only {
        build_mcp_widget();
    } else {
        // Even in --rust-only mode, ensure the asset exists
        let widget_asset = Path::new("crates/runt-mcp/assets/_output.html");
        if !widget_asset.exists() {
            eprintln!("MCP widget asset missing — building it first...");
            build_mcp_widget();
        }
    }

    // Phase 1: Single cargo invocation for all binaries.
    // Building all packages together ensures workspace feature unification
    // happens in one pass, so the later `cargo tauri build` finds everything
    // cached instead of recompiling the entire dependency tree.
    println!("Building all Rust targets (runtimed, runt, mcp-supervisor, notebook)...");
    run_cmd(
        "cargo",
        &[
            "build",
            "-p",
            "runtimed",
            "-p",
            "runt-cli",
            "-p",
            "mcp-supervisor",
            "-p",
            "notebook",
        ],
    );

    // Copy sidecar binaries for Tauri bundling
    copy_sidecar_binary("runtimed", false);
    copy_sidecar_binary("runt", false);

    // Phase 2: Run independent tasks in parallel.
    // - Python env sync + maturin develop (builds .so for MCP server)
    // - MCP Apps widget (self-contained HTML bundled into the nteract package)
    // - Frontend build (pnpm/vite, completely independent of Rust)
    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();

    handles.push(thread::spawn(|| {
        ensure_python_env();
        ensure_maturin_develop();
    }));

    // Note: build_mcp_widget() already ran in Phase 0 above.

    if rust_only {
        let dist_dir = Path::new("apps/notebook/dist");
        if !dist_dir.exists() {
            eprintln!("Error: No frontend build found at apps/notebook/dist");
            eprintln!("Run `cargo xtask build` (without --rust-only) first.");
            exit(1);
        }
        println!("Skipping frontend build (--rust-only), reusing existing assets");
    } else {
        handles.push(thread::spawn(|| {
            println!("Building frontend (notebook)...");
            run_frontend_build(true);
        }));
    }

    for handle in handles {
        handle.join().unwrap_or_else(|_| {
            eprintln!("A parallel build task panicked");
            exit(1);
        });
    }

    // Phase 3: Tauri build. With all Rust already compiled and frontend
    // assets in place, this is mostly a link step.
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
    eprintln!("Note: 'build-e2e' is deprecated, use 'cargo xtask e2e build' instead.");
    cmd_e2e_build();
}

fn print_e2e_help() {
    eprintln!("Usage: cargo xtask e2e [COMMAND]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!(
        "  build                          Build the E2E binary (debug, with embedded WebDriver)"
    );
    eprintln!("  test                           Run E2E smoke tests (default if no command given)");
    eprintln!("  test-fixture <notebook> <spec>  Run a single fixture test");
    eprintln!("  test-all                       Run smoke + all fixture tests");
    eprintln!("  help                           Show this help");
}

fn cmd_e2e(args: Vec<String>) {
    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("test");
    match subcmd {
        "build" => cmd_e2e_build(),
        "test" => cmd_e2e_test(args),
        "test-fixture" => cmd_e2e_test_fixture(args),
        "test-all" => cmd_e2e_test_all(),
        "help" | "--help" | "-h" => {
            print_e2e_help();
        }
        _ => {
            eprintln!("Unknown e2e subcommand: {subcmd}");
            eprintln!();
            print_e2e_help();
            exit(1);
        }
    }
}

fn cmd_e2e_build() {
    require_pnpm();
    require_tauri();

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
            "e2e-webdriver",
            "--config",
            r#"{"build":{"beforeBuildCommand":""}}"#,
        ],
    );

    println!("Build complete: ./target/debug/notebook");
    println!("The app embeds a WebDriver server on port 4445 (tauri-plugin-webdriver).");
}

/// Run a single E2E test session. Returns the test process exit code.
///
/// Spawns a dev daemon and the notebook app, waits for WebDriver on port
/// 4445, runs `pnpm test:e2e`, then cleans everything up.
fn run_e2e_session(
    notebook_path: Option<&str>,
    spec_path: Option<&str>,
    workspace_dir: Option<&str>,
) -> i32 {
    // Ensure e2e binary exists
    if !Path::new("./target/debug/notebook").exists() {
        cmd_e2e_build();
    }

    // Start daemon
    let mut daemon = if let Some(ws) = workspace_dir {
        // Custom workspace: spawn daemon with overridden RUNTIMED_WORKSPACE_PATH
        ensure_dev_daemon_binaries();
        let mut cmd = Command::new(dev_daemon_binary(false));
        cmd.args(["--dev", "run"])
            .env("RUNTIMED_DEV", "1")
            .env("RUNTIMED_WORKSPACE_PATH", ws)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap_or_else(|e| {
            eprintln!("Failed to start daemon: {e}");
            exit(1);
        });
        relay_child_output("daemon", child.stdout.take());
        relay_child_output("daemon", child.stderr.take());
        // Can't use wait_for_dev_daemon with a non-default workspace, poll briefly
        println!("Waiting for daemon to initialize...");
        thread::sleep(Duration::from_secs(10));
        child
    } else {
        let mut d = spawn_dev_daemon_process(false);
        if let Err(msg) = wait_for_dev_daemon(&mut d, Duration::from_secs(30)) {
            eprintln!("Failed to start dev daemon: {msg}");
            stop_child(&mut d, "daemon");
            return 1;
        }
        d
    };

    // Start the notebook app (embeds WebDriver on port 4445)
    let mut app_cmd = Command::new("./target/debug/notebook");
    if let Some(path) = notebook_path {
        app_cmd.arg(path);
    }
    app_cmd.env("RUST_LOG", "info");
    if let Some(ws) = workspace_dir {
        app_cmd
            .env("RUNTIMED_DEV", "1")
            .env("RUNTIMED_WORKSPACE_PATH", ws)
            .current_dir(ws);
    } else {
        apply_worktree_env(&mut app_cmd, true);
    }
    app_cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut app = match app_cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Failed to start notebook app: {e}");
            stop_child(&mut daemon, "daemon");
            return 1;
        }
    };
    relay_child_output("app", app.stdout.take());
    relay_child_output("app", app.stderr.take());

    // Wait for embedded WebDriver server on port 4445
    println!("Waiting for WebDriver on port 4445...");
    let wd_start = Instant::now();
    let wd_timeout = Duration::from_secs(30);
    let mut wd_ready = false;
    while wd_start.elapsed() < wd_timeout {
        if std::net::TcpStream::connect("127.0.0.1:4445").is_ok() {
            println!("WebDriver ready ({}s)", wd_start.elapsed().as_secs());
            wd_ready = true;
            break;
        }
        if app.try_wait().ok().flatten().is_some() {
            eprintln!("App exited before WebDriver became ready.");
            stop_child(&mut daemon, "daemon");
            return 1;
        }
        thread::sleep(Duration::from_secs(1));
    }
    if !wd_ready {
        eprintln!("Timed out waiting for WebDriver on port 4445.");
        stop_child(&mut app, "app");
        stop_child(&mut daemon, "daemon");
        return 1;
    }

    // Run pnpm test:e2e
    let mut test_cmd = Command::new("pnpm");
    test_cmd.args(["test:e2e"]).env("WEBDRIVER_PORT", "4445");
    if let Some(spec) = spec_path {
        test_cmd.env("E2E_SPEC", spec);
    }
    if let Some(ws) = workspace_dir {
        test_cmd.env("RUNTIMED_WORKSPACE_PATH", ws);
    }

    let test_code = match test_cmd.status() {
        Ok(s) => {
            if s.success() {
                0
            } else {
                s.code().unwrap_or(1)
            }
        }
        Err(e) => {
            eprintln!("Failed to run pnpm test:e2e: {e}");
            1
        }
    };

    // Cleanup
    stop_child(&mut app, "app");
    stop_child(&mut daemon, "daemon");

    test_code
}

fn cmd_e2e_test(_args: Vec<String>) {
    println!("Running E2E smoke tests...");
    let code = run_e2e_session(None, None, None);
    exit(code);
}

fn cmd_e2e_test_fixture(args: Vec<String>) {
    let notebook_path = args.get(1).unwrap_or_else(|| {
        eprintln!("Usage: cargo xtask e2e test-fixture <notebook_path> <spec_path>");
        exit(1);
    });
    let spec_path = args.get(2).unwrap_or_else(|| {
        eprintln!("Usage: cargo xtask e2e test-fixture <notebook_path> <spec_path>");
        exit(1);
    });

    println!("Running E2E fixture test...");
    println!("  Notebook: {notebook_path}");
    println!("  Spec:     {spec_path}");

    let code = run_e2e_session(Some(notebook_path), Some(spec_path), None);
    exit(code);
}

fn cmd_e2e_test_all() {
    println!("Running all E2E tests...\n");
    let mut failed = false;

    // 1. Smoke tests (default specs, excluding fixtures)
    println!("=== Smoke Tests ===");
    if run_e2e_session(None, None, None) != 0 {
        eprintln!("Smoke tests failed.");
        failed = true;
    }

    // 2. Fixture tests (mirroring CI e2e-fixtures job)
    let fixtures: &[(&str, &str, &str)] = &[
        (
            "crates/notebook/fixtures/audit-test/14-cell-visibility.ipynb",
            "e2e/specs/cell-visibility.spec.js",
            "Cell Visibility Test",
        ),
        (
            "crates/notebook/fixtures/audit-test/1-vanilla.ipynb",
            "e2e/specs/prewarmed-uv.spec.js",
            "Prewarmed Pool Test",
        ),
        (
            "crates/notebook/fixtures/audit-test/10-deno.ipynb",
            "e2e/specs/deno.spec.js",
            "Deno Kernel Test",
        ),
    ];

    for (notebook, spec, name) in fixtures {
        println!("\n=== {name} ===");
        if run_e2e_session(Some(notebook), Some(spec), None) != 0 {
            eprintln!("{name} failed.");
            failed = true;
        }
    }

    // 3. Untitled pyproject test (needs custom workspace directory)
    println!("\n=== Untitled Pyproject Test ===");
    let fixture_dir =
        std::fs::canonicalize("crates/notebook/fixtures/audit-test/pyproject-project")
            .unwrap_or_else(|e| {
                eprintln!("Failed to resolve pyproject fixture directory: {e}");
                exit(1);
            });
    let fixture_str = fixture_dir.to_string_lossy().to_string();
    if run_e2e_session(
        None,
        Some("e2e/specs/untitled-pyproject.spec.js"),
        Some(&fixture_str),
    ) != 0
    {
        eprintln!("Untitled Pyproject Test failed.");
        failed = true;
    }

    if failed {
        eprintln!("\nSome E2E tests failed.");
        exit(1);
    }
    println!("\nAll E2E tests passed!");
}

fn cmd_wasm() {
    require_tool("wasm-pack", WASM_PACK_INSTALL);

    println!("Building runtimed-wasm...");
    run_cmd(
        "wasm-pack",
        &[
            "build",
            "crates/runtimed-wasm",
            "--target",
            "web",
            "--out-dir",
            "../../apps/notebook/src/wasm/runtimed-wasm",
        ],
    );
    println!("WASM build complete. Output: apps/notebook/src/wasm/runtimed-wasm/");
}

fn cmd_icons(source: Option<&str>) {
    require_tauri();

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
    require_pnpm();
    require_tauri();

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
    // Guard: warn if running from a feature branch or worktree to prevent
    // accidentally replacing the system daemon with a dev build.
    if let Ok(branch) = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
    {
        let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
        if branch != "main" && !branch.is_empty() {
            eprintln!("⚠️  You are on branch '{branch}', not 'main'.");
            eprintln!("   This will install your local build as the system daemon,");
            eprintln!("   replacing the current nightly/release version.");
            eprintln!();
            eprintln!("   For development, use: cargo xtask dev-daemon");
            eprintln!("   Press Ctrl+C within 5 seconds to abort...");
            eprintln!();
            std::thread::sleep(Duration::from_secs(5));
        }
    }

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
        let _ = runt_workspace::launchd_stop();
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
        if let Err(e) = runt_workspace::launchd_start() {
            eprintln!("Warning: failed to start launchd service: {e}");
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
fn cmd_mcp(print_config: bool, release: bool) {
    ensure_python_env();
    ensure_maturin_develop();

    // Build the daemon in the requested mode so the supervisor finds it
    if release {
        println!("Building runtimed (release) for supervisor...");
        run_cmd("cargo", &["build", "--release", "-p", "runtimed"]);
        run_cmd("cargo", &["build", "--release", "-p", "runt-cli"]);
    }

    if print_config {
        // Build the supervisor, then run it with --print-config
        // For now, print the config pointing at the binary
        run_cmd("cargo", &["build", "-p", "mcp-supervisor"]);
        let binary = if cfg!(windows) {
            "target/debug/mcp-supervisor.exe"
        } else {
            "target/debug/mcp-supervisor"
        };
        let binary_path = fs::canonicalize(binary).unwrap_or_else(|e| {
            eprintln!("Failed to resolve supervisor binary path: {e}");
            exit(1);
        });
        let config = if release {
            serde_json::json!({
                "command": binary_path.to_string_lossy(),
                "env": {
                    "RUNTIMED_DEV": "1",
                    "RUNTIMED_RELEASE": "1"
                }
            })
        } else {
            serde_json::json!({
                "command": binary_path.to_string_lossy(),
                "env": {
                    "RUNTIMED_DEV": "1"
                }
            })
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&config).unwrap_or_else(|e| {
                eprintln!("Failed to serialize config: {e}");
                exit(1);
            })
        );
        return;
    }

    // Build and exec the supervisor binary
    run_cmd("cargo", &["build", "-p", "mcp-supervisor"]);
    let binary = if cfg!(windows) {
        "target/debug/mcp-supervisor.exe"
    } else {
        "target/debug/mcp-supervisor"
    };

    let mut command = Command::new(binary);
    apply_worktree_env(&mut command, true);
    if release {
        command.env("RUNTIMED_RELEASE", "1");
    }

    let status = command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run mcp-supervisor: {e}");
        exit(1);
    });

    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
}

fn cmd_dev_mcp(print_config: bool) {
    // Step 1: Build the runt CLI so we can query daemon status
    if !Path::new(dev_runt_cli_binary()).exists() {
        println!("Building runt CLI...");
        run_cmd("cargo", &["build", "-p", "runt-cli"]);
    }

    // Step 2: Resolve the socket path from the dev daemon
    let socket_path = {
        let mut command = Command::new(dev_runt_cli_binary());
        command
            .args(["daemon", "status", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_worktree_env(&mut command, true);

        let output = command.output().unwrap_or_else(|e| {
            eprintln!("Failed to run runt daemon status: {e}");
            eprintln!("Build the CLI first: cargo build -p runt-cli");
            exit(1);
        });

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("runt daemon status failed:");
            if !stderr.trim().is_empty() {
                eprintln!("{}", stderr.trim());
            }
            exit(1);
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            eprintln!("Failed to parse daemon status JSON: {e}");
            exit(1);
        });

        let path = json
            .get("socket_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| {
                eprintln!("No socket_path in daemon status output");
                exit(1);
            })
            .to_string();

        let running = json
            .get("running")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if !running && !print_config {
            eprintln!("Warning: dev daemon is not running.");
            eprintln!("Start it first: cargo xtask dev-daemon");
            eprintln!();
        }

        path
    };

    // Step 3: Sync Python workspace + build native bindings
    ensure_python_env();
    ensure_maturin_develop();

    // Step 4: Print config or launch
    let workspace_dir = fs::canonicalize(".").unwrap_or_else(|e| {
        eprintln!("Failed to resolve workspace directory: {e}");
        exit(1);
    });

    if print_config {
        let config = serde_json::json!({
            "command": "uv",
            "args": ["run", "--no-sync", "--directory", workspace_dir.to_string_lossy(), "nteract"],
            "env": {
                "RUNTIMED_SOCKET_PATH": socket_path
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&config).unwrap_or_else(|e| {
                eprintln!("Failed to serialize MCP config: {e}");
                exit(1);
            })
        );
    } else {
        println!();
        println!("Launching nteract MCP server...");
        println!("Socket: {socket_path}");
        println!();

        let status = Command::new("uv")
            .args([
                "run",
                "--no-sync",
                "--directory",
                &workspace_dir.to_string_lossy(),
                "nteract",
            ])
            .env("RUNTIMED_SOCKET_PATH", &socket_path)
            .status()
            .unwrap_or_else(|e| {
                eprintln!("Failed to launch nteract MCP server: {e}");
                exit(1);
            });

        if !status.success() {
            exit(status.code().unwrap_or(1));
        }
    }
}

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
    runt_workspace::default_vite_port()
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

    // Rust clippy (always check-only — clippy doesn't auto-fix)
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

    // Python with ruff (if uv is available and pyproject.toml exists at root)
    if Path::new("pyproject.toml").exists() {
        if Command::new("uv").arg("--version").output().is_ok() {
            println!("=== Python (ruff) ===");

            // ruff check
            let check_args = if fix {
                vec!["run", "ruff", "check", "--fix", "."]
            } else {
                vec!["run", "ruff", "check", "."]
            };
            let check_status = Command::new("uv").args(&check_args).status();
            if !check_status.map(|s| s.success()).unwrap_or(false) {
                failed = true;
            }

            // ruff format
            let format_args = if fix {
                vec!["run", "ruff", "format", "."]
            } else {
                vec!["run", "ruff", "format", "--check", "."]
            };
            let format_status = Command::new("uv").args(&format_args).status();
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
        apply_sccache_env(&mut command);
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

    // Build with appropriate profile
    if release {
        run_cmd("cargo", &["build", "--release", "-p", package]);
    } else {
        run_cmd("cargo", &["build", "-p", package]);
    }

    copy_sidecar_binary(binary_name, release);
}

/// Copy an already-built binary to the sidecar locations for Tauri bundling.
/// Copies to both `crates/notebook/binaries/` (for bundle builds) and
/// `target/{debug,release}/binaries/` (for no-bundle dev builds).
fn copy_sidecar_binary(binary_name: &str, release: bool) {
    let target = get_host_target();
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
        apply_sccache_env(&mut command);
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

/// Check that the WASM binary is a real WebAssembly file, not an unresolved
/// Git LFS pointer. A pointer file starts with "version https://git-lfs"
/// and is ~130 bytes; the real WASM starts with the magic bytes `\0asm`.
fn ensure_wasm_resolved() {
    let wasm = Path::new("apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm");
    let bytes = match fs::read(wasm) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("Error: failed to read {}: {e}", wasm.display());
            eprintln!("       The frontend build requires this WASM file.");
            eprintln!();
            eprintln!("  If you just cloned:  git lfs install && git lfs pull");
            eprintln!("  To rebuild from source:  wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm");
            exit(1);
        }
    };
    if bytes.starts_with(b"\0asm") {
        return; // real WebAssembly binary
    }
    if bytes.starts_with(b"version https://git-lfs") {
        eprintln!("Error: runtimed_wasm_bg.wasm is a Git LFS pointer, not the actual binary.");
    } else {
        eprintln!("Error: runtimed_wasm_bg.wasm is not a valid WebAssembly binary.");
    }
    eprintln!("       The frontend build will fail without the real file.");
    eprintln!();
    eprintln!("  Fix:  git lfs install && git lfs pull");
    eprintln!();
    eprintln!("  If you don't have git-lfs:");
    eprintln!("    macOS:  brew install git-lfs");
    eprintln!("    Linux:  sudo apt install git-lfs  (or see https://git-lfs.com)");
    exit(1);
}

/// Build the MCP Apps widget (apps/mcp-app) and copy it into the Python
/// nteract package so it ships with the PyPI wheel.
fn build_mcp_widget() {
    println!("Building MCP Apps widget...");
    run_cmd("pnpm", &["--filter", "nteract-mcp-app", "install"]);
    run_cmd("pnpm", &["--filter", "nteract-mcp-app", "run", "build"]);
    let dest = Path::new("python/nteract/src/nteract/_widget.html");
    if !dest.exists() {
        eprintln!("Error: MCP widget build did not produce _widget.html");
        exit(1);
    }
    println!("MCP Apps widget built successfully");
}

fn run_frontend_build(debug_bundle: bool) {
    ensure_wasm_resolved();
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

/// Set `RUSTC_WRAPPER=sccache` when sccache is available.
///
/// Skips detection entirely if `RUSTC_WRAPPER` is already set in the
/// environment (respects existing tooling). Detection runs `sccache
/// --version` once and caches the result for the lifetime of the process.
fn apply_sccache_env(command: &mut Command) {
    if env::var_os("RUSTC_WRAPPER").is_some() {
        return;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *AVAILABLE.get_or_init(|| {
        let found = Command::new("sccache")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if found {
            println!("Using sccache for compilation cache");
        }
        found
    });
    if available {
        command.env("RUSTC_WRAPPER", "sccache");
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
}

fn exit_on_failed_status(label: &str, status: ExitStatus) {
    if !status.success() {
        eprintln!("{label} exited with status {status}");
        exit(status.code().unwrap_or(1));
    }
}

/// Package nteract as a Claude Desktop extension (.mcpb ZIP archive).
///
/// The bundle contains:
///   manifest.json   — metadata and server entry point
///   icon.png        — 512×512 light-theme icon
///   icon-dark.png   — 512×512 dark-theme icon
///
/// The server is NOT bundled as a binary. Instead the manifest includes a
/// Node launcher script that finds the `runt` (or `runt-nightly`) binary
/// on the user's PATH or in well-known install locations, then execs
/// `runt mcp` for stdio transport.
///
/// Manifest templates live in `mcpb/manifest.{variant}.json`. The only
/// substitution is `{{VERSION}}` → the `runtimed` crate version.
fn cmd_mcpb(output: Option<&str>, variant: &str) {
    let version = read_package_version("runtimed");

    // ── 1. Read and populate the manifest template ──────────────────────────
    let template_path = format!("mcpb/manifest.{variant}.json");
    let template = fs::read_to_string(&template_path).unwrap_or_else(|e| {
        eprintln!("Failed to read {template_path}: {e}");
        eprintln!("Valid variants: stable, nightly (looked for mcpb/manifest.{{variant}}.json)");
        exit(1);
    });

    let manifest_str = template.replace("{{VERSION}}", &version);

    // Parse to validate JSON and re-serialize with consistent formatting.
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap_or_else(|e| {
        eprintln!("Invalid JSON in {template_path} after substitution: {e}");
        exit(1);
    });
    let manifest_str = serde_json::to_string_pretty(&manifest).unwrap_or_else(|e| {
        eprintln!("Failed to serialize manifest.json: {e}");
        exit(1);
    });

    // ── 2. Create a staging directory ───────────────────────────────────────
    let staging_dir = std::env::temp_dir().join(format!("nteract-mcpb-{}", std::process::id()));
    fs::create_dir_all(&staging_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create staging directory: {e}");
        exit(1);
    });

    // ── 3. Copy icons ────────────────────────────────────────────────────────
    // Stable: light = source.png, dark = source-nightly.png
    // Nightly: light = source-nightly.png, dark = source.png (swapped)
    let (light_src, dark_src) = match variant {
        "nightly" => (
            "crates/notebook/icons/source-nightly.png",
            "crates/notebook/icons/source.png",
        ),
        _ => (
            "crates/notebook/icons/source.png",
            "crates/notebook/icons/source-nightly.png",
        ),
    };

    if !Path::new(light_src).exists() {
        eprintln!("Icon not found: {light_src}");
        eprintln!("Run `cargo xtask icons` first to generate icons.");
        let _ = fs::remove_dir_all(&staging_dir);
        exit(1);
    }

    // Resize icons to 512x512 — source assets are 1024x1024 but the manifest
    // declares 512x512 and Claude Desktop may be strict about the match.
    let resize_icon = |src: &str, dest: &str| {
        let status = Command::new("sips")
            .args(["-z", "512", "512", src, "--out", dest])
            .stdout(Stdio::null())
            .status()
            .unwrap_or_else(|e| {
                eprintln!("Failed to run sips to resize {src}: {e}");
                exit(1);
            });
        if !status.success() {
            eprintln!("sips failed to resize {src}");
            exit(1);
        }
    };

    let light_dest = staging_dir.join("icon.png");
    resize_icon(light_src, &light_dest.to_string_lossy());

    // If the dark icon doesn't exist, fall back to the light icon.
    let dark_actual = if Path::new(dark_src).exists() {
        dark_src
    } else {
        light_src
    };
    let dark_dest = staging_dir.join("icon-dark.png");
    resize_icon(dark_actual, &dark_dest.to_string_lossy());

    // ── 4. Copy server launcher ────────────────────────────────────────────
    let server_dir = staging_dir.join("server");
    fs::create_dir_all(&server_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create server directory: {e}");
        exit(1);
    });
    fs::copy("mcpb/server/launch.js", server_dir.join("launch.js")).unwrap_or_else(|e| {
        eprintln!("Failed to copy server/launch.js: {e}");
        exit(1);
    });

    // ── 5. Write manifest.json ──────────────────────────────────────────────
    fs::write(staging_dir.join("manifest.json"), &manifest_str).unwrap_or_else(|e| {
        eprintln!("Failed to write manifest.json: {e}");
        exit(1);
    });

    // ── 6. Create ZIP archive ────────────────────────────────────────────────
    let default_name = if variant == "stable" {
        "nteract.mcpb"
    } else {
        "nteract-nightly.mcpb"
    };
    let output_path = output.unwrap_or(default_name);

    // Resolve the output path to an absolute path before changing directories.
    let abs_output = if Path::new(output_path).is_absolute() {
        Path::new(output_path).to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|e| {
                eprintln!("Failed to get current directory: {e}");
                exit(1);
            })
            .join(output_path)
    };

    // Ensure the parent directory exists so zip can create the output file.
    if let Some(parent) = abs_output.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!(
                "Failed to create output directory {}: {e}",
                parent.display()
            );
            exit(1);
        });
    }

    // Remove any existing archive so zip doesn't merge old contents.
    let _ = fs::remove_file(&abs_output);

    println!("Creating archive {}...", abs_output.display());

    let zip_status = Command::new("zip")
        .args(["-r", &abs_output.to_string_lossy(), "."])
        .current_dir(&staging_dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run zip: {e}");
            eprintln!("zip must be available in PATH.");
            exit(1);
        });

    if !zip_status.success() {
        eprintln!("zip command failed");
        let _ = fs::remove_dir_all(&staging_dir);
        exit(1);
    }

    // ── 7. Cleanup staging dir ───────────────────────────────────────────────
    let _ = fs::remove_dir_all(&staging_dir);

    println!("Done: {}", abs_output.display());
}

/// Read the version of a workspace package from `cargo metadata`.
fn read_package_version(package: &str) -> String {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run cargo metadata: {e}");
            exit(1);
        });

    if !output.status.success() {
        eprintln!("cargo metadata failed");
        exit(1);
    }

    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        eprintln!("Failed to parse cargo metadata output: {e}");
        exit(1);
    });

    metadata["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"].as_str() == Some(package)))
        .and_then(|p| p["version"].as_str())
        .unwrap_or("0.0.0")
        .to_string()
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
        let port = runt_workspace::vite_port_for_workspace(workspace);
        assert_eq!(port, runt_workspace::vite_port_for_workspace(workspace));
        assert!((5100u16..10000u16).contains(&port));
    }
}
