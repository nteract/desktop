//! Daemon-scoped cache for the vendored launcher script.
//!
//! `uv run` (and `pixi exec`) create ephemeral envs per invocation, so
//! we can't vendor into them the way we do for inline/prewarmed/conda/
//! pixi pool envs. Instead we stash a copy of `LAUNCHER_SRC` at a stable
//! cache path and inject its directory via `PYTHONPATH` so `-m
//! nteract_kernel_launcher` resolves inside the child interpreter.
//!
//! Invoking the launcher via `python {path}` would set `sys.path[0]` to
//! the launcher's cache dir and shadow the notebook's cwd for sibling
//! imports, so we deliberately keep `-m` + `PYTHONPATH` instead.
//!
//! Written once per daemon process on first access. Subsequent callers
//! reuse the path. Idempotent across daemon restarts (we overwrite on
//! every first-access per process).

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use kernel_env::launcher::{LAUNCHER_FILENAME, LAUNCHER_SRC};

static CACHED_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Return a stable path to a file containing `LAUNCHER_SRC`.
///
/// The file lives under `<daemon_base_dir>/launcher/nteract_kernel_launcher.py`.
/// Created on first call per daemon process; reused thereafter.
///
/// Prefer [`launcher_cache_dir`] for PYTHONPATH injection. This helper
/// exists primarily to materialize the on-disk file so the directory
/// returned by `launcher_cache_dir` actually contains the module.
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

/// Return the directory containing the cached launcher script, suitable
/// for injecting as `PYTHONPATH` so `-m nteract_kernel_launcher` resolves
/// without changing `sys.path[0]`.
///
/// This preserves Python's default cwd-first `sys.path` semantics, which
/// is important for notebooks that import sibling modules from the
/// project directory.
pub async fn launcher_cache_dir() -> Result<PathBuf> {
    let path = launcher_script_path().await?;
    path.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("launcher script path {path:?} has no parent"))
}
