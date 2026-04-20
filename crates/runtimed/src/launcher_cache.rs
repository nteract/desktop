//! Daemon-scoped cache for the vendored launcher script.
//!
//! `uv run` creates an ephemeral venv per invocation, so we can't vendor
//! into it the way we do for inline/prewarmed/conda/pixi envs. Instead we
//! stash a copy of `LAUNCHER_SRC` at a stable cache path and hand `uv run`
//! the script path so `python {path}` executes the launcher directly.
//!
//! Written once per daemon process on first access. Subsequent callers
//! reuse the path. Idempotent across daemon restarts (we overwrite on
//! every first-access per process).

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use kernel_env::launcher::{LAUNCHER_FILENAME, LAUNCHER_SRC};

static CACHED_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Return a stable path to a file containing `LAUNCHER_SRC`, suitable
/// for passing to `python {path}` via `uv run`.
///
/// The file lives under `<daemon_base_dir>/launcher/nteract_kernel_launcher.py`.
/// Created on first call per daemon process; reused thereafter.
pub async fn launcher_script_path() -> Result<PathBuf> {
    if let Some(p) = CACHED_PATH.get() {
        return Ok(p.clone());
    }
    let dir = runt_workspace::daemon_base_dir().join("launcher");
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create launcher cache dir {dir:?}"))?;
    let path = dir.join(LAUNCHER_FILENAME);
    tokio::fs::write(&path, LAUNCHER_SRC)
        .await
        .with_context(|| format!("write launcher to {path:?}"))?;
    let _ = CACHED_PATH.set(path.clone());
    Ok(path)
}
