// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{exit, Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

mod bump;

/// Find the workspace root (nearest ancestor containing a Cargo.toml with
/// a `[workspace]` section). Subcommands that need repo-relative paths
/// can call `ensure_workspace_root_cwd()` from within the subcommand to
/// `cd` there before shelling out.
fn find_workspace_root() -> Option<PathBuf> {
    let mut dir = env::current_dir().ok()?;
    loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists() {
            if let Ok(contents) = fs::read_to_string(&cargo) {
                if contents.contains("[workspace]") {
                    return Some(dir);
                }
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Change the process cwd to the workspace root. Scope this to the
/// specific subcommands that need it — not the top of `main` — because
/// several xtask subcommands accept user-supplied relative path arguments
/// (`notebook foo.ipynb`, `icons ./src.png`, `mcpb --output dist/out.mcpb`,
/// `e2e test-fixture fixture.ipynb spec.js`, `run notebook.ipynb`) and
/// those must stay relative to the shell cwd where the user invoked
/// `cargo xtask`. A global cd silently reinterprets those args against
/// the workspace root and opens/writes the wrong files.
pub(crate) fn ensure_workspace_root_cwd() {
    if let Some(root) = find_workspace_root() {
        let _ = env::set_current_dir(&root);
    }
}

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
        "build-dmg" => cmd_build_dmg(),
        "build-app" => cmd_build_app(),
        "dev-daemon" => {
            let release = args.iter().any(|a| a == "--release");
            cmd_dev_daemon(release);
        }
        "run-mcp" | "mcp" => {
            let print_config = args.iter().any(|a| a == "--print-config");
            let release = args.iter().any(|a| a == "--release");
            cmd_mcp(print_config, release);
        }
        "mcp-inspector" => cmd_mcp_inspector(),
        "lint" => {
            let fix = args.iter().any(|a| a == "--fix");
            cmd_lint(fix);
        }
        "clippy" => cmd_clippy(),
        "integration" => {
            let filter = args.iter().find(|a| !a.starts_with('-')).cloned();
            cmd_integration(filter);
        }
        "wasm" => {
            let skip_renderer_plugins = args.iter().any(|a| a == "--skip-renderer-plugins");
            let target = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .map(|s| s.as_str());
            cmd_wasm(target, skip_renderer_plugins);
        }
        "renderer-plugins" => cmd_renderer_plugins(),
        "verify-plugins" => cmd_verify_plugins(),
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
        "sync-tool-cache" => {
            let check = args.iter().any(|a| a == "--check");
            cmd_sync_tool_cache(check);
        }
        "check-dep-budget" => cmd_check_dep_budget(),
        "bump" => {
            let level = args.get(1).map(String::as_str).unwrap_or("patch");
            bump::cmd_bump(level);
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
  dev-daemon [--release]     Build and run runtimed in per-worktree dev mode
                             (for local nightly install on Linux see ./scripts/install-nightly)

MCP:
  run-mcp [--release]        Build and run the nteract-dev MCP supervisor (proxy + daemon + auto-restart)
  run-mcp --print-config     Print MCP client config JSON (for Zed, Claude, etc.)
  mcp-inspector              Launch MCPJam Inspector UI to test runt mcp (MCP Apps)

Linting:
  lint                       Check formatting and linting (Rust fmt, JS/TS, Python)
  lint --fix                 Auto-fix formatting and linting issues
  clippy                     Run cargo clippy (excludes runtimed-py; CI covers it)

Testing:
  integration [filter]       Run Python integration tests with an isolated daemon
                             Optional filter is passed to pytest -k (e.g. 'test_start_kernel')
  e2e [build|test|test-fixture|test-all]
                             E2E testing (build, run, manage fixtures)

Other:
  wasm                       Rebuild all WASM targets (runtimed-wasm + sift-wasm).
                             If sift-wasm was (re)built, also runs renderer-plugins
                             so the sift.js bundles re-embed the fresh wasm-bindgen glue.
  wasm runtimed              Rebuild only runtimed-wasm
  wasm sift                  Rebuild only sift-wasm (bindings for @nteract/sift);
                             also copies the binary to crates/runt-mcp/assets/plugins/
                             and rebuilds renderer plugins.
  wasm --skip-renderer-plugins
                             Skip the chained renderer-plugins rebuild (escape hatch
                             for intentionally testing drift between the two).
  renderer-plugins           Rebuild pre-built renderer plugins (notebook + MCP)
  verify-plugins             Check renderer plugin bundles match their wasm artifacts
                             (every wasm-bindgen import in the plugin JS must be
                             exported by the paired wasm binary). Catches #2048-style
                             drift without requiring cross-platform byte reproducibility.
  icons [source.png]         Generate icon variants
  mcpb                       Package nteract as a Claude Desktop extension (.mcpb)
  mcpb --variant nightly     Build nightly variant (different name/icon)
  mcpb --output <path>       Write the .mcpb archive to a custom path
  sync-tool-cache            Regenerate tool-cache.json + MCPB manifests from runt binary
  sync-tool-cache --check    Check caches are up to date + description byte budget (for CI)
  check-dep-budget           Check transitive dependency counts against per-crate budgets
  bump [patch|minor|major]   Bump every versioned artifact (crates, Tauri app,
                             Python, frontend packages, plugin manifests) in
                             lockstep and regenerate Cargo.lock. Defaults to patch.
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

    // dx integration tests are NOT run here — they require the real repo
    // pyproject.toml (kernels use `env_source="uv:pyproject"` to install dx
    // and pandas), but `cmd_integration` deliberately uses a TEMP
    // RUNTIMED_WORKSPACE_PATH for isolation. dx integration is gated by
    // the CI workflow `build.yml` instead, which runs from
    // `${GITHUB_WORKSPACE}` (the real repo root).
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
    require_tool(pnpm_bin(), PNPM_INSTALL);
}

/// Name to invoke pnpm under for `Command::new`.
///
/// On Windows, `pnpm/action-setup@v4` (and corepack) install pnpm as a
/// `pnpm.cmd` shim wrapping node. There is no `pnpm.exe`. Rust's
/// `Command::new("pnpm")` calls `CreateProcess` directly, which does not
/// apply `PATHEXT` resolution, so it fails with "not found in PATH" even
/// when `pnpm.cmd` is on `PATH` and `pnpm install` works fine from the
/// surrounding bash/pwsh shell. Always invoke `pnpm.cmd` on Windows.
fn pnpm_bin() -> &'static str {
    if cfg!(windows) {
        "pnpm.cmd"
    } else {
        "pnpm"
    }
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

    // `cargo tauri dev` boots its own Vite dev server and recompiles the
    // notebook crate fresh, so the Phase 2 production bundle and Phase 3
    // tauri build that `cargo xtask build` runs would be ~40s of warm work
    // whose output the dev path immediately discards. Skip both. We still
    // need:
    //   - the gitignored wasm + renderer-plugin outputs (Vite imports
    //     them as virtual modules)
    //   - the MCP widget HTML (`runt-mcp` `include_str!`s `_output.html`)
    //   - the runtimed/runt/nteract-mcp sidecars when the dev daemon
    //     spawn falls through below
    ensure_build_artifacts();
    if skip_build {
        println!("Skipping sidecar build (--skip-build)");
    } else {
        build_mcp_widget();
        ensure_dev_daemon_binaries();
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

    // Vite's isolated-renderer plugin reads the gitignored renderer-plugin
    // outputs as virtual modules; missing files become empty strings and
    // every plugin renders blank. Build them once on fresh clones.
    ensure_build_artifacts();

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

    for link in [Path::new("packages/sift/node_modules/@chenglou/pretext")] {
        if !link.exists() {
            return Some("workspace package links missing");
        }
    }

    let Some(install_time) = modified_time(install_marker) else {
        return Some("could not read node_modules metadata timestamp");
    };
    for manifest in [
        Path::new("package.json"),
        Path::new("pnpm-lock.yaml"),
        Path::new("pnpm-workspace.yaml"),
        Path::new("apps/mcp-app/package.json"),
        Path::new("apps/notebook/package.json"),
        Path::new("apps/renderer-test/package.json"),
        Path::new("packages/notebook-host/package.json"),
        Path::new("packages/runtimed-node/package.json"),
        Path::new("packages/runtimed/package.json"),
        Path::new("packages/sift/package.json"),
    ] {
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

const PYTHON_SYNC_STAMP: &str = "target/uv/.sync-stamp";

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
            Ok(s) if s.success() => {
                let stamp = Path::new(PYTHON_SYNC_STAMP);
                if let Some(parent) = stamp.parent() {
                    if let Err(e) = fs::create_dir_all(parent).and_then(|_| fs::write(stamp, "")) {
                        eprintln!("Warning: failed to write Python sync stamp: {e}");
                    }
                }
            }
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

    let Some(sync_time) = modified_time(Path::new(PYTHON_SYNC_STAMP)) else {
        return Some("missing Python sync stamp");
    };

    for manifest in [
        Path::new("uv.lock"),
        Path::new("pyproject.toml"),
        Path::new("python/nteract/pyproject.toml"),
        Path::new("python/runtimed/pyproject.toml"),
        venv_marker,
    ] {
        if let Some(manifest_time) = modified_time(manifest) {
            if manifest_time > sync_time {
                return Some("pyproject.toml or uv.lock changed");
            }
        } else {
            return Some("could not read Python env timestamps");
        }
    }

    None
}

const MATURIN_DEVELOP_STAMP: &str = "target/maturin/.develop-stamp";

fn maturin_develop_reason() -> Option<&'static str> {
    let stamp_time = modified_time(Path::new(MATURIN_DEVELOP_STAMP));
    let watched_times = [
        latest_modified_time_under(Path::new("Cargo.lock")),
        latest_modified_time_under(Path::new("crates/runtimed-py/Cargo.toml")),
        latest_modified_time_under(Path::new("crates/runtimed-py/src")),
        latest_modified_time_under(Path::new("crates/runtimed/Cargo.toml")),
        latest_modified_time_under(Path::new("crates/runtimed/src")),
        latest_modified_time_under(Path::new("crates/runtimed-client/Cargo.toml")),
        latest_modified_time_under(Path::new("crates/runtimed-client/src")),
        latest_modified_time_under(Path::new("python/runtimed/pyproject.toml")),
        latest_modified_time_under(Path::new("python/runtimed/src")),
        latest_modified_time_under(Path::new("pyproject.toml")),
        latest_modified_time_under(Path::new(".venv/pyvenv.cfg")),
    ];
    freshness_reason(stamp_time, watched_times)
}

fn freshness_reason<I>(stamp_time: Option<SystemTime>, watched_times: I) -> Option<&'static str>
where
    I: IntoIterator<Item = Option<SystemTime>>,
{
    let Some(stamp_time) = stamp_time else {
        return Some("missing develop stamp");
    };

    for watched_time in watched_times {
        let Some(watched_time) = watched_time else {
            return Some("could not read binding source timestamps");
        };
        if watched_time > stamp_time {
            return Some("binding sources changed");
        }
    }

    None
}

fn latest_modified_time_under(path: &Path) -> Option<SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;

    if metadata.is_dir() {
        for entry in fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            let entry_latest = latest_modified_time_under(&entry.path())?;
            latest = latest.max(entry_latest);
        }
    }

    Some(latest)
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

    let Some(reason) = maturin_develop_reason() else {
        println!("Skipping maturin develop (bindings are up to date).");
        return;
    };

    println!("Building runtimed Python bindings (maturin develop, {reason})...");
    // Resolve absolute path — maturin warns on relative VIRTUAL_ENV.
    // cargo xtask always runs from the workspace root (all paths in this
    // file are relative to it), so current_dir() is the repo root.
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("Warning: failed to get current directory for maturin develop");
        return;
    };
    // Use a separate target directory so maturin's cdylib build doesn't
    // invalidate fingerprints in the main target/ dir. Without this,
    // cargo tauri build (Phase 3) sees stale timestamps from maturin's
    // concurrent writes and recompiles the entire dependency tree.
    let maturin_target = cwd.join("target/maturin");
    let status = Command::new("uv")
        .args([
            "run",
            "--directory",
            "python/runtimed",
            "maturin",
            "develop",
            "--target-dir",
            &maturin_target.to_string_lossy(),
        ])
        .env("VIRTUAL_ENV", cwd.join(".venv"))
        .env_remove("CONDA_PREFIX")
        .status();

    match status {
        Ok(s) if s.success() => {
            let stamp = maturin_target.join(".develop-stamp");
            if let Err(e) = fs::create_dir_all(&maturin_target).and_then(|_| fs::write(&stamp, ""))
            {
                eprintln!("Warning: failed to write maturin develop stamp: {e}");
            }
        }
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

    // Phase 0: Ensure the gitignored wasm + renderer-plugin outputs exist.
    // `runtimed`'s build.rs `include_bytes!`-embeds them; the frontend
    // virtual modules need them too. On a fresh clone this triggers a one-
    // time `cargo xtask wasm`; on warm trees it's a free file-existence
    // check.
    ensure_build_artifacts();

    // Phase 0a: Install workspace pnpm deps up front. This was previously
    // kicked off as a background thread alongside cargo build (which made
    // sense when the MCP widget step below didn't also shell out to pnpm),
    // but `build_mcp_widget` runs `pnpm --filter <pkg> install` which
    // assumes the workspace is already initialized. On a fresh clone with
    // no `node_modules`, that filtered install would spin up the root
    // install itself — redundant at best, flaky at worst. Do it once,
    // here, before anyone else touches pnpm.
    if !rust_only {
        ensure_pnpm_install();
    }

    // Phase 0b: Build the MCP widget HTML before any Rust compilation.
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

    // Phase 1: Build all Rust crates except `notebook`.
    // The `notebook` crate's build.rs declares `rerun-if-changed` on
    // `apps/notebook/dist`, so building it here would be wasted work —
    // Phase 2 rebuilds the frontend (updating dist/), which invalidates
    // notebook's fingerprint and forces cargo tauri build to recompile it
    // anyway. By excluding notebook here, we still pre-warm the entire
    // dependency tree (all shared crates are built via the other targets),
    // and Phase 3 only needs to compile notebook + link.
    println!("Building Rust targets (runtimed, runt, nteract-mcp, mcp-supervisor)...");
    run_cmd(
        "cargo",
        &[
            "build",
            "-p",
            "runtimed",
            "-p",
            "runt",
            "-p",
            "nteract-mcp",
            "-p",
            "mcp-supervisor",
        ],
    );

    // Copy sidecar binaries for Tauri bundling
    copy_sidecar_binary("runtimed", false);
    copy_sidecar_binary("runt", false);
    copy_sidecar_binary("nteract-mcp", false);

    // Phase 2: Build the frontend. Python bindings are no longer part of
    // the default build — `runt mcp` is Rust-native, and agents iterating
    // on runtimed-py should use `cargo xtask integration` (which runs
    // `maturin develop`) or rebuild via the nteract-dev MCP (`up
    // rebuild=true`). CI still runs maturin explicitly in build.yml.
    if rust_only {
        let dist_dir = Path::new("apps/notebook/dist");
        if !dist_dir.exists() {
            eprintln!("Error: No frontend build found at apps/notebook/dist");
            eprintln!("Run `cargo xtask build` (without --rust-only) first.");
            exit(1);
        }
        println!("Skipping frontend build (--rust-only), reusing existing assets");
    } else {
        println!("Building frontend (notebook)...");
        run_frontend_build(true);
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

    // pnpm build runs: notebook UI. Set `VITE_E2E=1` so the bundler
    // keeps the E2E-only test bridge (`window.__nteractWidgetUpdate`,
    // `window.__nteractWidgetStore`) in the output — it's gated on
    // `import.meta.env.VITE_E2E` in `App.tsx` so production bundles
    // without this env var don't expose it.
    println!("Building frontend (notebook)...");
    std::env::set_var("VITE_E2E", "1");
    run_frontend_build(true);
    std::env::remove_var("VITE_E2E");

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
        (
            "crates/notebook/fixtures/audit-test/16-widget-slider.ipynb",
            "e2e/specs/widget-slider-stall.spec.js",
            "Widget Slider Stall Reproducer",
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

fn cmd_wasm(target: Option<&str>, skip_renderer_plugins: bool) {
    // `wasm-pack build crates/<name>` and the subsequent `fs::copy`/
    // `fs::read_dir` calls here all use repo-relative paths. cd to the
    // workspace root so this works whether the user invoked xtask from
    // the root, from `packages/sift`, or from anywhere else.
    ensure_workspace_root_cwd();
    require_tool("wasm-pack", WASM_PACK_INSTALL);

    // Default (no target) builds both. `sift` or `runtimed` pick just one.
    let (build_runtimed, build_sift) = match target {
        None | Some("--all") => (true, true),
        Some("sift") => (false, true),
        Some("runtimed") => (true, false),
        Some(other) => {
            eprintln!("Unknown wasm target: {other}. Use 'sift', 'runtimed', or '--all'.");
            std::process::exit(1);
        }
    };

    if build_runtimed {
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
        let _ = fs::remove_file("apps/notebook/src/wasm/runtimed-wasm/.gitignore");
        println!("WASM build complete. Output: apps/notebook/src/wasm/runtimed-wasm/");
    }

    if build_sift {
        println!("Building sift-wasm...");
        // Build to the canonical wasm-pack output location
        // (crates/sift-wasm/pkg/) — this is where
        //   - packages/sift/vite.config.ts aliases `sift-wasm`
        //   - packages/sift/vitest.config.ts looks for real glue
        //   - src/build/renderer-plugin-builder.ts::resolveWasmGlue()
        //     looks for real glue (falls back to __mocks__ stub otherwise)
        // expect it. If this path is empty, the renderer plugin bundles
        // the mock stub and sift renders "sift-wasm not built" at runtime.
        run_cmd(
            "wasm-pack",
            &[
                "build",
                "crates/sift-wasm",
                "--target",
                "web",
                "--release",
                // Default --out-dir (./pkg) is what all consumers expect.
            ],
        );
        let _ = fs::remove_file("crates/sift-wasm/pkg/.gitignore");
        // Mirror the pkg to packages/sift/public/wasm/ for the sift demo
        // app's runtime fetch (vite base=/, served as static asset).
        let pkg_dir = Path::new("crates/sift-wasm/pkg");
        let public_dir = Path::new("packages/sift/public/wasm");
        if let Err(e) = fs::create_dir_all(public_dir) {
            eprintln!("Warning: failed to create {}: {e}", public_dir.display());
        } else {
            for entry in fs::read_dir(pkg_dir).into_iter().flatten().flatten() {
                let src = entry.path();
                let Some(name) = src.file_name() else {
                    continue;
                };
                let dest = public_dir.join(name);
                if let Err(e) = fs::copy(&src, &dest) {
                    eprintln!(
                        "Warning: failed to copy {} → {}: {e}",
                        src.display(),
                        dest.display()
                    );
                }
            }
        }
        // Copy the WASM binary to the daemon's embedded plugins directory so
        // `/plugins/sift_wasm.wasm` serves the freshly-built binary. The
        // destination directory is gitignored, so create it before copying
        // (fresh checkouts won't have it yet). Failure here is fatal:
        // runtimed's `include_bytes!` will silently embed a stale binary
        // (or fail to compile if absent) on the next cargo build.
        let src = Path::new("crates/sift-wasm/pkg/sift_wasm_bg.wasm");
        let dest = Path::new("crates/runt-mcp/assets/plugins/sift_wasm.wasm");
        if let Some(parent) = dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                panic!(
                    "Failed to create {}: {e}. Re-run after fixing permissions.",
                    parent.display()
                );
            }
        }
        if let Err(e) = fs::copy(src, dest) {
            panic!(
                "Failed to copy {} to {}: {e}. \
                 wasm-pack output may be missing or the destination unwritable.",
                src.display(),
                dest.display()
            );
        }
        println!("Copied {} -> {}", src.display(), dest.display());
        println!(
            "WASM build complete. Output: crates/sift-wasm/pkg/ (mirrored to packages/sift/public/wasm/)"
        );
    }

    // Renderer plugin bundles embed wasm-bindgen glue from sift-wasm (the
    // `__wbg_*_<hash>` import names). Rebuilding sift-wasm without rebuilding
    // the plugins leaves them pointing at stale names — see #2048 (the
    // `__wbg_call_<hash> must be callable` runtime error). Chain the plugin
    // build by default so that class of drift can't happen via this command.
    //
    // `--skip-renderer-plugins` is the escape hatch for cases where you
    // explicitly want the drift (e.g. reproducing a bug against an older
    // bundle, or CI steps that intentionally test one half of the chain).
    if build_sift && !skip_renderer_plugins {
        println!();
        cmd_renderer_plugins();
    }
}

fn cmd_renderer_plugins() {
    // The `node scripts/build-renderer-plugins.ts` call below resolves
    // the script path against cwd — normalize it so this works from
    // any subdirectory.
    ensure_workspace_root_cwd();
    require_pnpm();
    ensure_pnpm_install();
    println!("Building renderer plugins...");
    // Build both the notebook renderer plugins and the runt-mcp plugin
    // assets. Uses the shared renderer-plugin-builder.ts to produce:
    //   - apps/notebook/src/renderer-plugins/ (IIFE + 4 CJS plugins, gitignored)
    //   - crates/runt-mcp/assets/plugins/    (MCP-wrapped plugins, gitignored)
    run_cmd(
        "node",
        &[
            "--experimental-strip-types",
            "scripts/build-renderer-plugins.ts",
        ],
    );
    println!("Renderer plugins built.");
    println!("  Notebook: apps/notebook/src/renderer-plugins/");
    println!("  MCP:      crates/runt-mcp/assets/plugins/");
    println!("Outputs are gitignored — CI rebuilds them as a prerequisite step.");
}

/// Verify renderer plugin bundles are coherent with their paired wasm binaries.
///
/// The #2048 failure mode looked like this: wasm was rebuilt (new mangled
/// `__wbg_call_<hash>` import names), but the plugin JS bundle was not.
/// Browser loads the plugin, plugin imports names that don't exist on the
/// current wasm, error: `import function ./sift_wasm_bg.js:__wbg_* must be
/// callable`.
///
/// Strict byte-level drift checks can't enforce this because wasm-pack output
/// is not bit-reproducible across macOS and Linux. Instead we check a weaker
/// but sufficient invariant: every wasm-bindgen import name referenced in the
/// plugin JS must be exported by the paired wasm binary.
///
/// If this ever has a false positive, the paired-file list or the symbol
/// regex is the place to look.
fn cmd_verify_plugins() {
    ensure_workspace_root_cwd();

    // (plugin bundle JS, paired wasm binary). Extend when a new wasm-backed
    // renderer plugin lands.
    let pairs: &[(&str, &str)] = &[
        (
            "crates/runt-mcp/assets/plugins/sift.js",
            "crates/runt-mcp/assets/plugins/sift_wasm.wasm",
        ),
        (
            "apps/notebook/src/renderer-plugins/sift.js",
            "crates/sift-wasm/pkg/sift_wasm_bg.wasm",
        ),
    ];

    let mut failed = false;
    for (plugin_js, wasm_path) in pairs {
        match verify_plugin_against_wasm(Path::new(plugin_js), Path::new(wasm_path)) {
            Ok(count) => {
                println!("  ok  {plugin_js} ({count} imports match {wasm_path})");
            }
            Err(msg) => {
                eprintln!("::error file={plugin_js}::{msg}");
                failed = true;
            }
        }
    }

    if failed {
        eprintln!();
        eprintln!(
            "Plugin bundle is out of sync with its wasm artifact. Run \
             `cargo xtask wasm && cargo xtask renderer-plugins` and commit \
             the updated files."
        );
        exit(1);
    }

    println!("All renderer plugin bundles match their wasm artifacts.");
}

/// Inspect a wasm file's imports, scanning for the wasm-bindgen glue names
/// (`__wbg_*`, `__wbindgen_*`). Hand-rolled so xtask doesn't pick up a wasm
/// parsing dep (keeps `cargo xtask lint` fast).
///
/// Format we walk (Web Assembly Core Spec 1.0, binary format):
///
/// ```text
/// Module := \0 a s m \x01 \0 \0 \0      magic + version
///           (SectionId:u8  Size:ULEB128  Payload[Size])*
/// ImportSection (id=2) payload := n:ULEB128  Import[n]
/// Import := module:Name  name:Name  desc:ImportDesc
/// Name   := len:ULEB128  bytes[len] (UTF-8)
/// ```
///
/// We only need the names, so once we find section id=2 we walk `n` imports,
/// read each pair of length-prefixed strings, and skip past the import
/// descriptor (type, table, memory, or global) without fully decoding it.
fn wasm_bindgen_imports(wasm_path: &Path) -> Result<Vec<String>, String> {
    let bytes =
        fs::read(wasm_path).map_err(|e| format!("failed to read {}: {e}", wasm_path.display()))?;
    parse_wasm_bindgen_imports(&bytes).map_err(|e| format!("{}: {e}", wasm_path.display()))
}

fn parse_wasm_bindgen_imports(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err("not a wasm file (missing \\0asm magic)".into());
    }
    // Skip magic (4) + version (4).
    let mut cur = Cursor::new(&bytes[8..]);

    loop {
        let Some(section_id) = cur.read_u8() else {
            return Ok(Vec::new()); // reached EOF with no import section
        };
        let section_size = cur.read_uleb128()? as usize;
        let section_end = cur.pos + section_size;
        if section_end > cur.buf.len() {
            return Err(format!(
                "section {section_id} length {section_size} overruns file"
            ));
        }

        if section_id == 2 {
            // Import section.
            let mut section = Cursor::new(&cur.buf[cur.pos..section_end]);
            let count = section.read_uleb128()? as usize;
            let mut out = Vec::new();
            for _ in 0..count {
                let _module = section.read_name()?;
                let name = section.read_name()?;
                section.skip_import_desc()?;
                if name.starts_with("__wbg_") || name.starts_with("__wbindgen_") {
                    out.push(name.to_string());
                }
            }
            return Ok(out);
        }

        cur.pos = section_end;
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn read_uleb128(&mut self) -> Result<u64, String> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = self
                .read_u8()
                .ok_or_else(|| "unexpected EOF in LEB128".to_string())?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 63 {
                return Err("LEB128 value too large".into());
            }
        }
    }

    fn read_name(&mut self) -> Result<&'a str, String> {
        let len = self.read_uleb128()? as usize;
        let end = self.pos + len;
        if end > self.buf.len() {
            return Err(format!(
                "name length {len} overruns buffer (pos={})",
                self.pos
            ));
        }
        let s = std::str::from_utf8(&self.buf[self.pos..end])
            .map_err(|e| format!("invalid UTF-8 in name: {e}"))?;
        self.pos = end;
        Ok(s)
    }

    /// Skip past an ImportDesc entry. Four shapes:
    ///   0x00 TypeIdx           (1 u32 index)
    ///   0x01 TableType         (elem_type u8, limits)
    ///   0x02 MemType           (limits)
    ///   0x03 GlobalType        (val_type u8, mut u8)
    /// Where limits = flags:u8, min:u32, and max:u32 if flags bit 0 is set.
    fn skip_import_desc(&mut self) -> Result<(), String> {
        let kind = self
            .read_u8()
            .ok_or_else(|| "unexpected EOF in import descriptor".to_string())?;
        match kind {
            0x00 => {
                self.read_uleb128()?;
            }
            0x01 => {
                // elem type (anyfunc/externref, 1 byte in MVP)
                self.read_u8()
                    .ok_or_else(|| "EOF in table elem type".to_string())?;
                self.skip_limits()?;
            }
            0x02 => {
                self.skip_limits()?;
            }
            0x03 => {
                self.read_u8()
                    .ok_or_else(|| "EOF in global val type".to_string())?;
                self.read_u8()
                    .ok_or_else(|| "EOF in global mutability".to_string())?;
            }
            other => return Err(format!("unknown import descriptor kind 0x{other:02x}")),
        }
        Ok(())
    }

    fn skip_limits(&mut self) -> Result<(), String> {
        let flags = self
            .read_u8()
            .ok_or_else(|| "EOF in limits flags".to_string())?;
        self.read_uleb128()?; // min
        if flags & 0x01 != 0 {
            self.read_uleb128()?; // max
        }
        Ok(())
    }
}

fn verify_plugin_against_wasm(plugin_js: &Path, wasm_path: &Path) -> Result<usize, String> {
    const WASM_URL_SENTINEL: &str = "__wasm_loaded_via_setWasmUrl__";

    if !plugin_js.exists() {
        return Err(format!("plugin bundle missing: {}", plugin_js.display()));
    }
    if !wasm_path.exists() {
        return Err(format!("wasm binary missing: {}", wasm_path.display()));
    }

    let imports = wasm_bindgen_imports(wasm_path)?;
    if imports.is_empty() {
        // Not an error — a wasm module may legitimately have no wasm-bindgen
        // imports. But flag it so we don't silently pass after a refactor
        // that accidentally empties the list.
        return Err(format!(
            "{} has no __wbg_/__wbindgen_ imports — refusing to vacuously pass the check",
            wasm_path.display()
        ));
    }

    let bundle = fs::read_to_string(plugin_js)
        .map_err(|e| format!("failed to read {}: {e}", plugin_js.display()))?;

    if !bundle.contains(WASM_URL_SENTINEL) {
        return Err(format!(
            "plugin bundle does not contain {WASM_URL_SENTINEL:?}; \
             exclude-wasm-inline may not have rewritten the wasm-bindgen URL"
        ));
    }

    let mut missing = Vec::new();
    for name in &imports {
        if !bundle.contains(name.as_str()) {
            missing.push(name.clone());
        }
    }

    if !missing.is_empty() {
        let preview: Vec<&String> = missing.iter().take(5).collect();
        return Err(format!(
            "plugin bundle does not reference {} wasm import name(s) from {}: e.g. {:?}",
            missing.len(),
            wasm_path.display(),
            preview,
        ));
    }

    Ok(imports.len())
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

    // Same artifact-existence check as cmd_build, so packaging a release
    // works on a fresh clone (CI) without an explicit `cargo xtask wasm`.
    ensure_build_artifacts();

    // Ensure pnpm workspace deps are installed before anything else touches
    // pnpm — `run_frontend_build` below and any tauri `beforeBuildCommand`
    // assume node_modules is populated. Fresh clones would otherwise fail
    // with missing-workspace-package errors.
    ensure_pnpm_install();

    // Generate icons if source exists
    let source_path = "crates/notebook/icons/source.png";
    if Path::new(source_path).exists() {
        cmd_icons(None);
    } else {
        println!("Skipping icon generation (no source.png found)");
    }

    // Build runtimed daemon binary for bundling (release mode for distribution)
    build_runtimed_daemon(true);

    // Generate the SMAppService launch agent plist for inclusion in the bundle.
    // This must happen before `cargo tauri build` so the plist is signed with
    // the app bundle (modifying Contents/ after signing invalidates the signature).
    generate_launch_agent_plist();

    // Build frontend
    println!("Building frontend...");
    run_frontend_build(false);

    // Build Tauri app
    println!("Building Tauri app ({bundle} bundle)...");
    let tauri_config = launch_agent_tauri_config();
    run_cmd(
        "cargo",
        &[
            "tauri",
            "build",
            "--bundles",
            bundle,
            "--config",
            &tauri_config,
        ],
    );

    println!("Build complete!");
}

/// Build a Tauri `--config` override JSON that:
/// 1. Disables `beforeBuildCommand` (we already built the frontend)
/// 2. Includes the launch agent plist in the macOS bundle
///
/// The plist is included at `Contents/Library/LaunchAgents/<label>.plist`
/// so SMAppService can find it. The files map is channel-specific since
/// the label differs between stable and nightly.
fn launch_agent_tauri_config() -> String {
    let label = runt_workspace::daemon_launchd_label();
    let plist_filename = format!("{label}.plist");
    let bundle_dest = format!("Library/LaunchAgents/{plist_filename}");
    let source_path = format!("./launch-agents/{plist_filename}");

    // Build the config JSON with serde_json to avoid escaping issues
    let config = serde_json::json!({
        "build": {
            "beforeBuildCommand": ""
        },
        "bundle": {
            "macOS": {
                "files": {
                    bundle_dest: source_path
                }
            }
        }
    });

    config.to_string()
}

/// Generate the launch agent plist for SMAppService registration.
///
/// On macOS 13+, SMAppService looks for the plist inside the app bundle at
/// `Contents/Library/LaunchAgents/<label>.plist`. This function generates the
/// plist with channel-specific values and writes it to `crates/notebook/launch-agents/`
/// where `tauri.conf.json` picks it up via `bundle.macOS.files`.
///
/// The plist uses `BundleProgram` (bundle-relative path) instead of absolute
/// `ProgramArguments`, as required by SMAppService.
#[allow(clippy::expect_used)] // xtask is a dev tool; panics with context are fine
fn generate_launch_agent_plist() {
    let label = runt_workspace::daemon_launchd_label();
    let daemon_binary = runt_workspace::daemon_binary_basename();

    let log_level = match runt_workspace::build_channel() {
        runt_workspace::BuildChannel::Nightly => {
            "info,notebook_sync=debug,runtimed::notebook_sync_server=debug"
        }
        runt_workspace::BuildChannel::Stable => "warn",
    };

    // BundleProgram is relative to the .app bundle root
    let bundle_program = format!("Contents/MacOS/{daemon_binary}");

    // Note: HOME, USER, StandardOutPath, and StandardErrorPath are omitted
    // because they require the user's home directory which isn't known at
    // build time. This plist is only used on macOS 13+ where launchd's
    // user-domain agent loading reliably sets HOME and USER. The daemon
    // also manages its own log file internally and falls back to /tmp.
    //
    // This plist is additive — the legacy ~/Library/LaunchAgents/ plist
    // (which includes HOME, USER, and ~/.local/bin in PATH) is always
    // also written at install time and is the primary one used by
    // launchctl start/stop.

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>BundleProgram</key>
    <string>{bundle_program}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bundle_program}</string>
        <string>--log-level</string>
        <string>{log_level}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
    </dict>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin</string>
    </dict>
</dict>
</plist>
"#,
    );

    let output_dir = Path::new("crates/notebook/launch-agents");
    fs::create_dir_all(output_dir).expect("Failed to create launch-agents directory");

    let plist_filename = format!("{label}.plist");
    let output_path = output_dir.join(&plist_filename);
    fs::write(&output_path, plist_content).expect("Failed to write launch agent plist");

    println!("Generated launch agent plist: {}", output_path.display());
}

/// Build and run runtimed in per-worktree development mode.
///
/// This enables isolated daemon instances per git worktree, useful when
/// developing/testing daemon code across multiple worktrees simultaneously.
fn cmd_mcp(print_config: bool, release: bool) {
    // Skip ensure_python_env/ensure_maturin_develop here — the supervisor
    // handles maturin develop asynchronously in its background init task.
    // Removing these saves 5-15s of startup time.

    // Build the daemon in the requested mode so the supervisor finds it
    if release {
        println!("Building runtimed (release) for supervisor...");
        run_cmd("cargo", &["build", "--release", "-p", "runtimed"]);
        run_cmd("cargo", &["build", "--release", "-p", "runt"]);
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
        let mut env_map = serde_json::Map::new();
        env_map.insert("RUNTIMED_DEV".into(), serde_json::json!("1"));
        if release {
            env_map.insert("RUNTIMED_RELEASE".into(), serde_json::json!("1"));
        }
        if let Some(path) = runt_workspace::get_workspace_path() {
            env_map.insert(
                "RUNTIMED_WORKSPACE_PATH".into(),
                serde_json::json!(path.to_string_lossy()),
            );
        }

        let config = serde_json::json!({
            "command": binary_path.to_string_lossy(),
            "env": env_map,
        });
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

fn cmd_mcp_inspector() {
    require_pnpm();

    // Build runt so it's ready when the inspector spawns it
    println!("Building runt CLI...");
    run_cmd("cargo", &["build", "-p", "runt"]);

    ensure_pnpm_install();

    let runt_binary = fs::canonicalize(dev_runt_cli_binary()).unwrap_or_else(|e| {
        eprintln!("Failed to resolve runt binary path: {e}");
        exit(1);
    });

    // Build a mcpServers config so nteract is pre-populated and auto-connects
    let mut env_map = serde_json::Map::new();
    env_map.insert("RUNTIMED_DEV".into(), serde_json::json!("1"));
    if let Some(path) = runt_workspace::get_workspace_path() {
        env_map.insert(
            "RUNTIMED_WORKSPACE_PATH".into(),
            serde_json::json!(path.to_string_lossy()),
        );
    }

    let config = serde_json::json!({
        "mcpServers": {
            "nteract": {
                "command": runt_binary.to_string_lossy(),
                "args": ["mcp"],
                "env": env_map,
            }
        }
    });

    let config_path = env::temp_dir().join("nteract-mcp-inspector.json");
    fs::write(&config_path, config.to_string()).unwrap_or_else(|e| {
        eprintln!("Failed to write inspector config: {e}");
        exit(1);
    });

    println!("Starting MCPJam Inspector...");
    println!("UI will open at http://localhost:6274");
    println!("Server: nteract (auto-connect)");
    println!("Ensure the dev daemon is running (cargo xtask dev-daemon).");
    println!();

    let config_str = config_path.to_string_lossy().to_string();
    let mut command = Command::new("pnpm");
    command.args([
        "exec",
        "inspector",
        "--config",
        &config_str,
        "--server",
        "nteract",
    ]);
    apply_worktree_env(&mut command, true);

    let status = command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run inspector: {e}");
        eprintln!("Ensure @mcpjam/inspector is in devDependencies and run `pnpm install`.");
        exit(1);
    });

    // Clean up temp config
    let _ = fs::remove_file(&config_path);

    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
}

fn cmd_dev_daemon(release: bool) {
    // runtimed's build.rs panics if the gitignored renderer-plugin
    // outputs are missing. Ensure they exist before kicking off the
    // cargo build so a fresh clone can run `cargo xtask dev-daemon`
    // directly.
    ensure_build_artifacts();

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

    let mut cmd = Command::new(binary);
    cmd.args(["--dev", "run"]);
    apply_worktree_env(&mut cmd, true);
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

    // Fast checks first — these finish in seconds and catch the most common issues.

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

    // JavaScript/TypeScript with Vite Plus
    println!("=== JavaScript/TypeScript (vp check) ===");
    let vp_ok = if fix {
        run_cmd_ok("vp", &["check", "--fix"])
    } else {
        run_cmd_ok("vp", &["check"])
    };
    if !vp_ok {
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

            // ty type-check. ty is a dev-dep at the workspace root; the
            // python-package workflow already gates PRs on it, so we run
            // the same command here to give local `cargo xtask lint` the
            // same coverage. `ty check` is read-only — the --fix flag has
            // no effect on it.
            println!("=== Python (ty) ===");
            let ty_status = Command::new("uv")
                .args(["run", "ty", "check", "python/"])
                .status();
            if !ty_status.map(|s| s.success()).unwrap_or(false) {
                failed = true;
            }
            println!();
        } else {
            println!("=== Python (ruff + ty) ===");
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

fn cmd_clippy() {
    println!("Running clippy...");
    println!();

    // Exclude runtimed-py to avoid the pyo3/maturin compile cost locally.
    // CI covers it in the runtimed-py-integration job.
    // Also exclude notebook (needs bundled sidecar binaries) and WASM crates
    // (need wasm-pack), matching CI's clippy-and-tests job.
    if !run_cmd_ok(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--exclude",
            "runtimed-py",
            "--exclude",
            "notebook",
            "--exclude",
            "runtimed-wasm",
            "--exclude",
            "sift-wasm",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ) {
        exit(1);
    }

    println!();
    println!("Clippy passed!");
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
    build_external_binary("runt", "runt", release);
    build_external_binary("nteract-mcp", "nteract-mcp", release);
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
    }
    if cmd == "wasm-pack" {
        strip_rustc_wrapper_for_wasm(&mut command);
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

/// Probe the gitignored wasm + renderer-plugin outputs and run `cargo xtask
/// wasm` once if anything is missing. One canonical file per output dir is
/// enough — `cmd_wasm` rebuilds the whole set in one shot, so we never have
/// to chase a partial state.
///
/// Called from the top of every entry point that compiles `runtimed`,
/// `notebook`, or runs the dev daemon, so a fresh clone can go straight to
/// `cargo xtask build` / `dev` / `dev-daemon` / `notebook` without an
/// explicit `cargo xtask wasm` step. `runtimed`'s `build.rs` panics with a
/// "run `cargo xtask wasm`" message if you bypass xtask and invoke `cargo
/// build` directly.
fn ensure_build_artifacts() {
    let probes = [
        "apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm",
        "crates/sift-wasm/pkg/sift_wasm_bg.wasm",
        "apps/notebook/src/renderer-plugins/sift.js",
        "crates/runt-mcp/assets/plugins/sift_wasm.wasm",
    ];
    let missing: Vec<&str> = probes
        .iter()
        .copied()
        .filter(|p| !Path::new(p).exists())
        .collect();
    if missing.is_empty() {
        // Sanity check: the runtimed-wasm binary should still start with the
        // WebAssembly magic. Catches the rare case of a truncated or stomped
        // output without forcing a full rebuild on every call.
        let wasm = Path::new("apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm");
        if let Ok(bytes) = fs::read(wasm) {
            if !bytes.starts_with(b"\0asm") {
                eprintln!(
                    "Warning: {} is not a valid WebAssembly binary; rebuilding via cargo xtask wasm",
                    wasm.display()
                );
                cmd_wasm(None, false);
            }
        }
        return;
    }

    println!("Build artifacts missing; running cargo xtask wasm first:");
    for p in &missing {
        println!("  - {p}");
    }
    cmd_wasm(None, false);
}

/// Build the MCP Apps widget (apps/mcp-app) and copy it into the Python
/// nteract package so it ships with the PyPI wheel.
fn mcp_widget_needs_rebuild() -> Option<&'static str> {
    let outputs = [
        Path::new("crates/runt-mcp/assets/_output.html"),
        Path::new("python/nteract/src/nteract/_widget.html"),
    ];

    // If any output is missing, must rebuild
    let mut oldest_output = None;
    for output in &outputs {
        if !output.exists() {
            return Some("output file missing");
        }
        let Some(t) = modified_time(output) else {
            return Some("could not read output timestamp");
        };
        oldest_output = Some(match oldest_output {
            None => t,
            Some(prev) => std::cmp::min(prev, t),
        });
    }
    // Safety: we checked all outputs exist above, so oldest_output is always Some
    let Some(oldest_output) = oldest_output else {
        return Some("could not determine output timestamps");
    };

    // Check build scripts, lockfile, and all source files against the oldest output.
    let top_level_sources = [
        Path::new("apps/mcp-app/package.json"),
        Path::new("apps/mcp-app/build-html.js"),
        Path::new("apps/mcp-app/vite.config.ts"),
        Path::new("apps/mcp-app/build-plugins.ts"),
        Path::new("src/build/renderer-plugin-builder.ts"),
        Path::new("pnpm-lock.yaml"),
    ];
    for src in &top_level_sources {
        if let Some(src_time) = modified_time(src) {
            if src_time > oldest_output {
                return Some("source files changed");
            }
        }
    }
    // Walk all files under apps/mcp-app/src/
    if let Ok(entries) = std::fs::read_dir("apps/mcp-app/src") {
        fn check_dir_recursive(dir: std::fs::ReadDir, threshold: std::time::SystemTime) -> bool {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Ok(sub) = std::fs::read_dir(&path) {
                        if check_dir_recursive(sub, threshold) {
                            return true;
                        }
                    }
                } else if let Some(t) = modified_time(&path) {
                    if t > threshold {
                        return true;
                    }
                }
            }
            false
        }
        if check_dir_recursive(entries, oldest_output) {
            return Some("source files changed");
        }
    }

    None
}

fn build_mcp_widget() {
    if let Some(reason) = mcp_widget_needs_rebuild() {
        println!("Building MCP Apps widget ({reason})...");
        run_cmd("pnpm", &["--filter", "nteract-mcp-app", "install"]);
        run_cmd("vp", &["run", "nteract-mcp-app#build"]);
        let dest = Path::new("python/nteract/src/nteract/_widget.html");
        if !dest.exists() {
            eprintln!("Error: MCP widget build did not produce _widget.html");
            exit(1);
        }
        println!("MCP Apps widget built successfully");
    } else {
        println!("Skipping MCP Apps widget build (outputs are up to date).");
    }
}

fn run_frontend_build(debug_bundle: bool) {
    ensure_build_artifacts();
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

fn strip_rustc_wrapper_for_wasm(command: &mut Command) {
    if env::var_os("RUSTC_WRAPPER").is_some() {
        eprintln!(
            "Note: stripping RUSTC_WRAPPER for wasm-pack \
             (sccache/mold/etc. break wasm32 C builds)"
        );
        command.env_remove("RUSTC_WRAPPER");
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

    // ── 4. Build and copy nteract-mcp binary ────────────────────────────────
    // Set RUNT_BUILD_CHANNEL so the binary knows its channel at compile time.
    let build_channel = match variant {
        "stable" => "stable",
        _ => "nightly",
    };
    println!("Building nteract-mcp (release, channel={build_channel})...");
    let mut build_cmd = Command::new("cargo");
    build_cmd.args(["build", "-p", "nteract-mcp", "--release"]);
    build_cmd.env("RUNT_BUILD_CHANNEL", build_channel);
    let build_status = build_cmd.status().unwrap_or_else(|e| {
        eprintln!("Failed to run cargo build -p nteract-mcp: {e}");
        exit(1);
    });
    if !build_status.success() {
        eprintln!("cargo build -p nteract-mcp --release failed");
        let _ = fs::remove_dir_all(&staging_dir);
        exit(1);
    }

    let binary_name = if cfg!(target_os = "windows") {
        "nteract-mcp.exe"
    } else {
        "nteract-mcp"
    };
    let built_binary = Path::new("target/release").join(binary_name);
    if !built_binary.exists() {
        eprintln!("Built binary not found at {}", built_binary.display());
        let _ = fs::remove_dir_all(&staging_dir);
        exit(1);
    }

    let server_dir = staging_dir.join("server");
    fs::create_dir_all(&server_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create server directory: {e}");
        exit(1);
    });
    fs::copy(&built_binary, server_dir.join(binary_name)).unwrap_or_else(|e| {
        eprintln!("Failed to copy nteract-mcp binary: {e}");
        exit(1);
    });

    // Strip the binary on Unix to minimize bundle size
    #[cfg(unix)]
    {
        let strip_target = server_dir.join(binary_name);
        let _ = Command::new("strip").arg(&strip_target).status();
    }

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

const TOOL_DESC_BYTE_BUDGET: usize = 1500;

#[allow(clippy::expect_used)]
fn cmd_sync_tool_cache(check: bool) {
    let tool_cache_path = Path::new("crates/runt-mcp-proxy/tool-cache.json");
    let manifest_nightly = Path::new("mcpb/manifest.nightly.json");
    let manifest_stable = Path::new("mcpb/manifest.stable.json");

    eprintln!("Building runt (release)...");
    run_cmd("cargo", &["build", "--release", "-p", "runt"]);

    eprintln!("Dumping tool list from runt mcp...");
    let runt_bin = Path::new("target/release/runt");
    let tools_json = dump_mcp_tools(runt_bin);

    // 3. Parse and compute description bytes
    let tools: serde_json::Value = serde_json::from_str(&tools_json)
        .unwrap_or_else(|e| panic!("Failed to parse tool list: {e}"));
    let tools_arr = tools.as_array().expect("tools should be an array");

    let total_desc_bytes: usize = tools_arr
        .iter()
        .filter_map(|t| t["description"].as_str())
        .map(|d| d.len())
        .sum();

    eprintln!(
        "  {} tools, {} description bytes (budget: {})",
        tools_arr.len(),
        total_desc_bytes,
        TOOL_DESC_BYTE_BUDGET
    );

    if total_desc_bytes > TOOL_DESC_BYTE_BUDGET {
        eprintln!(
            "ERROR: Tool description bytes ({}) exceed budget ({})",
            total_desc_bytes, TOOL_DESC_BYTE_BUDGET
        );
        if check {
            exit(1);
        } else {
            eprintln!("  (continuing anyway since --check was not passed)");
        }
    }

    // 4. Format the tool cache JSON
    let formatted = serde_json::to_string_pretty(tools_arr).expect("Failed to format tool cache");

    if check {
        // Check mode: compare against existing files
        let mut stale = false;

        let existing_cache = fs::read_to_string(tool_cache_path).unwrap_or_default();
        if existing_cache.trim() != formatted.trim() {
            eprintln!("STALE: {}", tool_cache_path.display());
            stale = true;
        }

        for manifest_path in [&manifest_nightly, &manifest_stable] {
            let existing = fs::read_to_string(manifest_path).unwrap_or_default();
            let updated = update_manifest_tools(&existing, tools_arr);
            if existing.trim() != updated.trim() {
                eprintln!("STALE: {}", manifest_path.display());
                stale = true;
            }
        }

        // Also check mcpb_install.rs descriptions match
        let mcpb_install = Path::new("crates/notebook/src/mcpb_install.rs");
        if mcpb_install.exists() {
            let source = fs::read_to_string(mcpb_install).unwrap_or_default();
            for tool in tools_arr {
                let name = tool["name"].as_str().unwrap_or("");
                let desc = tool["description"].as_str().unwrap_or("");
                // The mcpb_install.rs source is Rust code with JSON inside a
                // serde_json::json!() macro, so inner quotes appear as \"
                // in the source file. Escape them in the needle to match.
                let escaped_desc = desc.replace('"', r#"\""#);
                let needle = format!(r#""description": "{}""#, escaped_desc);
                if !source.contains(&needle) && source.contains(&format!(r#""name": "{}""#, name)) {
                    eprintln!(
                        "STALE: {} (description mismatch for tool '{}')",
                        mcpb_install.display(),
                        name
                    );
                    stale = true;
                    break;
                }
            }
        }

        if stale {
            eprintln!();
            eprintln!("Run `cargo xtask sync-tool-cache` to fix JSON caches.");
            eprintln!("If mcpb_install.rs is stale, update it manually to match.");
            exit(1);
        }
        eprintln!("All tool caches are up to date.");
    } else {
        // Write mode: update files
        fs::write(tool_cache_path, &formatted)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", tool_cache_path.display()));
        eprintln!("  Updated {}", tool_cache_path.display());

        for manifest_path in [&manifest_nightly, &manifest_stable] {
            let existing = fs::read_to_string(manifest_path).unwrap_or_default();
            let updated = update_manifest_tools(&existing, tools_arr);
            fs::write(manifest_path, &updated)
                .unwrap_or_else(|e| panic!("Failed to write {}: {e}", manifest_path.display()));
            eprintln!("  Updated {}", manifest_path.display());
        }

        eprintln!("Done. Review the changes and commit.");
    }
}

#[allow(clippy::expect_used)]
fn dump_mcp_tools(runt_bin: &Path) -> String {
    use std::io::{Read, Write};

    let mut child = Command::new(runt_bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn runt mcp");

    let stdin = child.stdin.as_mut().expect("stdin");
    let init_msg = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"xtask","version":"1.0"}}}"#;
    writeln!(stdin, "{}", init_msg).ok();
    thread::sleep(Duration::from_secs(1));

    let list_msg = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    writeln!(stdin, "{}", list_msg).ok();
    thread::sleep(Duration::from_secs(1));
    drop(child.stdin.take());

    let mut output = String::new();
    child
        .stdout
        .as_mut()
        .expect("stdout")
        .read_to_string(&mut output)
        .ok();
    child.kill().ok();
    child.wait().ok();

    for line in output.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(tools) = v.get("result").and_then(|r| r.get("tools")) {
                return serde_json::to_string(tools).expect("serialize tools");
            }
        }
    }
    panic!("Failed to get tools/list response from runt mcp");
}

#[allow(clippy::expect_used)]
fn update_manifest_tools(manifest_json: &str, tools: &[serde_json::Value]) -> String {
    let mut manifest: serde_json::Value =
        serde_json::from_str(manifest_json).expect("parse manifest");

    let tool_entries: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t["name"],
                "description": t["description"]
            })
        })
        .collect();

    manifest["tools"] = serde_json::Value::Array(tool_entries);

    let mut buf = serde_json::to_string_pretty(&manifest).expect("format manifest");
    buf.push('\n');
    buf
}

fn cmd_check_dep_budget() {
    ensure_workspace_root_cwd();

    // Budgets are unique crate counts (deduplicated, excluding the root crate).
    const BUDGETS: &[(&str, usize)] = &[
        ("xtask", 30),
        ("runt-workspace", 35),
        ("runt-trust", 50),
        ("notebook-doc", 110),
        ("kernel-launch", 175),
        ("runtimed-client", 310),
    ];

    println!("{:<20} {:>5}  {:>6}  Status", "Crate", "Deps", "Budget");

    let mut any_over = false;

    for &(crate_name, budget) in BUDGETS {
        let output = Command::new("cargo")
            .args(["tree", "-e", "normal", "--prefix", "none", "-p", crate_name])
            .output()
            .unwrap_or_else(|e| {
                eprintln!("Failed to run cargo tree for {crate_name}: {e}");
                exit(1);
            });

        if !output.status.success() {
            eprintln!(
                "cargo tree failed for {crate_name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            exit(1);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut seen = std::collections::HashSet::new();
        let mut count = 0usize;
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Strip trailing (*) duplicate markers
            let line = line.trim_end_matches(" (*)").trim();
            // Use the crate name + version (first two tokens) as the dedup key
            let key: String = line
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            if seen.insert(key) {
                count += 1;
            }
        }
        // Subtract the root crate itself
        count = count.saturating_sub(1);

        let (status, over) = if count > budget {
            (format!("\u{2717} OVER BUDGET (+{})", count - budget), true)
        } else {
            ("\u{2713}".to_string(), false)
        };

        println!("{:<20} {:>5}  {:>6}  {}", crate_name, count, budget, status);

        if over {
            any_over = true;
        }
    }

    if any_over {
        eprintln!();
        eprintln!("One or more crates exceed their dependency budget.");
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::UNIX_EPOCH;

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

    #[test]
    fn freshness_reason_requires_stamp() {
        let watched = [Some(UNIX_EPOCH + Duration::from_secs(5))];
        assert_eq!(
            freshness_reason(None, watched),
            Some("missing develop stamp")
        );
    }

    #[test]
    fn freshness_reason_detects_newer_sources() {
        let stamp = UNIX_EPOCH + Duration::from_secs(5);
        let watched = [Some(UNIX_EPOCH + Duration::from_secs(6))];
        assert_eq!(
            freshness_reason(Some(stamp), watched),
            Some("binding sources changed")
        );
    }

    #[test]
    fn freshness_reason_detects_missing_timestamps() {
        let stamp = UNIX_EPOCH + Duration::from_secs(5);
        let watched = [None];
        assert_eq!(
            freshness_reason(Some(stamp), watched),
            Some("could not read binding source timestamps")
        );
    }

    #[test]
    fn freshness_reason_skips_when_stamp_is_newer() {
        let stamp = UNIX_EPOCH + Duration::from_secs(10);
        let watched = [
            Some(UNIX_EPOCH + Duration::from_secs(5)),
            Some(UNIX_EPOCH + Duration::from_secs(9)),
        ];
        assert_eq!(freshness_reason(Some(stamp), watched), None);
    }

    /// Build a minimal wasm module with an import section. Used to exercise
    /// `parse_wasm_bindgen_imports` without depending on a real wasm-pack
    /// artifact.
    fn encode_uleb128(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
                out.push(byte);
            } else {
                out.push(byte);
                return out;
            }
        }
    }

    fn make_import_section(imports: &[(&str, &str)]) -> Vec<u8> {
        let mut body = encode_uleb128(imports.len() as u64);
        for (module, name) in imports {
            body.extend(encode_uleb128(module.len() as u64));
            body.extend_from_slice(module.as_bytes());
            body.extend(encode_uleb128(name.len() as u64));
            body.extend_from_slice(name.as_bytes());
            // import kind 0x00 (func) + type index 0
            body.push(0x00);
            body.extend(encode_uleb128(0));
        }
        let mut out = vec![0x02]; // section id = 2 (Import)
        out.extend(encode_uleb128(body.len() as u64));
        out.extend(body);
        out
    }

    fn make_wasm(imports: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::from(b"\0asm\x01\0\0\0" as &[u8]);
        out.extend(make_import_section(imports));
        out
    }

    #[test]
    fn parse_wasm_bindgen_imports_rejects_non_wasm() {
        assert!(parse_wasm_bindgen_imports(b"not a wasm file").is_err());
    }

    #[test]
    fn parse_wasm_bindgen_imports_returns_only_wasm_bindgen_names() {
        let wasm = make_wasm(&[
            ("./sift_wasm_bg.js", "__wbg_Error_abc123"),
            ("./sift_wasm_bg.js", "__wbindgen_throw"),
            ("env", "memory"), // non-wbg import, should be filtered
            ("./sift_wasm_bg.js", "__wbg_new_xyz789"),
        ]);
        let names = parse_wasm_bindgen_imports(&wasm).unwrap();
        assert_eq!(
            names,
            vec!["__wbg_Error_abc123", "__wbindgen_throw", "__wbg_new_xyz789"]
        );
    }

    #[test]
    fn parse_wasm_bindgen_imports_empty_when_no_imports() {
        let wasm = Vec::from(b"\0asm\x01\0\0\0" as &[u8]);
        assert_eq!(
            parse_wasm_bindgen_imports(&wasm).unwrap(),
            Vec::<String>::new()
        );
    }
}
