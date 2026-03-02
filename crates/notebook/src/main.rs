use clap::Parser;
use notebook::Runtime;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "notebook", about = "Open notebooks")]
struct Args {
    /// Path to notebook file to open or create
    path: Option<PathBuf>,

    /// Runtime for new notebooks (python, deno). Falls back to user settings if not specified.
    #[arg(long, short)]
    runtime: Option<Runtime>,

    /// Working directory for untitled notebooks (used for project file detection).
    /// When opening an untitled notebook, this directory will be used to find
    /// pyproject.toml, pixi.toml, or environment.yaml.
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Start a built-in WebDriver server on this port for E2E testing.
    /// Enables native E2E tests without Docker or tauri-driver.
    #[cfg(feature = "webdriver-test")]
    #[arg(long)]
    webdriver_port: Option<u16>,
}

fn main() {
    let args = Args::parse();

    #[cfg(feature = "webdriver-test")]
    let webdriver_port = args.webdriver_port;
    #[cfg(not(feature = "webdriver-test"))]
    let webdriver_port: Option<u16> = None;

    notebook::run(args.path, args.runtime, args.cwd, webdriver_port).expect("notebook app failed");
}
