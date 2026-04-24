//! Daemon-scoped cache for the vendored launcher package.
//!
//! `uv run` (and `pixi exec`) create ephemeral envs per invocation, so
//! we can't vendor into them the way we do for inline/prewarmed/conda/
//! pixi pool envs. Instead we stash a copy of the launcher package at a
//! stable cache path and inject its parent directory via `PYTHONPATH` so
//! `-m nteract_kernel_launcher` resolves inside the child interpreter.
//!
//! Invoking the launcher via `python {path}/__main__.py` would set
//! `sys.path[0]` to the launcher's cache dir and shadow the notebook's
//! cwd for sibling imports, so we deliberately keep `-m` + `PYTHONPATH`
//! instead.
//!
//! Written once per daemon process on first access. Subsequent callers
//! reuse the path. Idempotent across daemon restarts (we overwrite on
//! every first-access per process).

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use kernel_env::launcher::{LAUNCHER_FILES, LAUNCHER_PKG};

static CACHED_PARENT: OnceLock<PathBuf> = OnceLock::new();

/// Return the directory containing the cached launcher **package**, suitable
/// for injecting as `PYTHONPATH` so `-m nteract_kernel_launcher` resolves
/// without changing `sys.path[0]`.
///
/// Layout: `<daemon_base_dir>/launcher/nteract_kernel_launcher/<*.py>`. The
/// returned path is `<daemon_base_dir>/launcher/`. Python walks `sys.path`
/// entries looking for the package by name.
///
/// Preserving Python's default cwd-first `sys.path` semantics matters for
/// notebooks that import sibling modules from the project directory.
pub async fn launcher_cache_dir() -> Result<PathBuf> {
    if let Some(p) = CACHED_PARENT.get() {
        return Ok(p.clone());
    }
    let parent = runt_workspace::daemon_base_dir().join("launcher");
    tokio::fs::create_dir_all(&parent)
        .await
        .with_context(|| format!("create launcher cache dir {parent:?}"))?;

    let pkg_dir = parent.join(LAUNCHER_PKG);
    tokio::fs::create_dir_all(&pkg_dir)
        .await
        .with_context(|| format!("create launcher package dir {pkg_dir:?}"))?;

    for (relpath, contents) in LAUNCHER_FILES {
        let path = pkg_dir.join(relpath);
        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("write launcher file {path:?}"))?;
    }

    let _ = CACHED_PARENT.set(parent.clone());
    Ok(parent)
}
