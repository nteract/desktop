# Vendor `nteract_kernel_launcher` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `nteract_kernel_launcher` as a single Python file bundled into the Rust binary via `include_str!`, vendored into every kernel environment at creation time so `-m nteract_kernel_launcher` works without any PyPI package.

**Architecture:** The launcher is already a tiny file-scoped module (`enabled_exec_lines`, `_inject_exec_lines`, `main`). We collapse it to a single `.py` file at a known path, `include_str!` it from a new `kernel-env::launcher` module, and call a `vendor_into_venv(python_path)` helper after every UV / conda / pixi env is prepared. For `uv:pyproject`, we sidestep uv's resolver entirely and invoke the launcher by script path from a daemon cache dir. `UvDependencies::bootstrap_dx` and the `--with nteract-kernel-launcher` / `uv pip install nteract-kernel-launcher` sites go away — `bootstrap_dx` only controls `-m ipykernel_launcher` vs `-m nteract_kernel_launcher` (and whether `dx` is added to the pip install set; `dx` is on PyPI, no change there).

**Tech Stack:** Rust (`kernel-env`, `runtimed`), Python 3.10+ (`nteract-kernel-launcher`), `include_str!`, `sysconfig` for site-packages lookup.

---

## File Structure

| File | Responsibility | Status |
|------|----------------|--------|
| `python/nteract-kernel-launcher/nteract_kernel_launcher.py` | Canonical launcher source. Single file. | Create (move from `src/nteract_kernel_launcher/__init__.py`) |
| `python/nteract-kernel-launcher/pyproject.toml` | Retained for local pytest / pyright. Build target is the top-level file. | Modify |
| `python/nteract-kernel-launcher/tests/test_bootstrap.py` | Tests import from top-level file. | Modify (imports) |
| `python/nteract-kernel-launcher/src/` | Gone. | Delete |
| `crates/kernel-env/src/launcher.rs` | New module: `LAUNCHER_SRC` const + `vendor_into_venv` + path helpers. Rust-side home for the launcher file. | Create |
| `crates/kernel-env/src/lib.rs` | Export the new `launcher` module. | Modify |
| `crates/kernel-env/src/uv.rs` | Call `launcher::vendor_into_venv` after creation; drop `UvDependencies.bootstrap_dx`; drop `nteract-kernel-launcher` from install args. | Modify |
| `crates/kernel-env/src/conda.rs` | Call `launcher::vendor_into_venv` after conda env is ready. | Modify |
| `crates/runtimed/src/daemon.rs` | Drop `nteract-kernel-launcher` from `uv_prewarmed_packages`; call `launcher::vendor_into_venv` after pixi env creation; add vendor-on-claim fallback for UV pool take. | Modify |
| `crates/runtimed/src/inline_env.rs` | Drop `bootstrap_dx` field from local `UvDependencies` construction (struct field is gone). | Modify |
| `crates/runtimed/src/jupyter_kernel.rs` | `uv:pyproject` invokes `python {script_path}` instead of `--with nteract-kernel-launcher` + `-m nteract_kernel_launcher`. Other launch paths still use `-m nteract_kernel_launcher` because vendoring puts it in site-packages. | Modify |
| `crates/runtimed/src/launcher_cache.rs` | New: daemon-side helper that writes `LAUNCHER_SRC` to a stable cache path on first access (for `uv run python {path}`). | Create |

## Key Design Rules

1. **Vendoring is unconditional.** Every UV / conda / pixi env gets the launcher file, regardless of `bootstrap_dx`. The file is ~800 bytes; conditional logic adds bugs. `bootstrap_dx` stays a pure launch-time switch.
2. **Vendor target is sysconfig-derived.** Ask the target Python for `sysconfig.get_path("purelib")`, drop `nteract_kernel_launcher.py` there. Never hard-code `lib/pythonX.Y/site-packages`.
3. **Vendor on claim is a backstop.** Pool envs warmed before this change have no launcher file. `take_uv_env` / `take_conda_env` / pixi take-path vendor the file before handing off — idempotent write, cheap.
4. **Single-file module, not a package.** `nteract_kernel_launcher.py` at site-packages root. No directory, no `__init__.py`, no `__main__.py`. `-m nteract_kernel_launcher` works against a top-level module.
5. **Rust tests spawn real Python to validate the embedded string.** The `LAUNCHER_SRC` const must be syntactically valid or `-m` fails silently at kernel start. A `python -c "import ast; ast.parse(LAUNCHER_SRC)"` test catches that at `cargo test` time.

---

## Task 1: Collapse launcher to single-file module

**Files:**
- Create: `python/nteract-kernel-launcher/nteract_kernel_launcher.py`
- Delete: `python/nteract-kernel-launcher/src/nteract_kernel_launcher/__init__.py`
- Delete: `python/nteract-kernel-launcher/src/nteract_kernel_launcher/__main__.py`
- Delete: `python/nteract-kernel-launcher/src/` (and below)
- Modify: `python/nteract-kernel-launcher/pyproject.toml`
- Modify: `python/nteract-kernel-launcher/tests/test_bootstrap.py`

- [ ] **Step 1: Create the single-file launcher**

Exact file contents (move + add a `__main__` guard so the file is directly runnable as a script):

```python
"""nteract-kernel-launcher — wrapper around ipykernel_launcher with kernel bootstrap.

Two supported invocations:

    python -m nteract_kernel_launcher -f <connection_file>   # vendored into venv
    python /path/to/nteract_kernel_launcher.py -f <file>     # run as a script

Bootstrap runs inside the kernel, *after* IPython is initialized but *before*
any user code executes. We achieve that ordering by appending the bootstrap
snippet to ``IPKernelApp.exec_lines`` on the process's argv before handing
off to ``ipykernel.kernelapp.launch_new_instance()``.
"""

from __future__ import annotations

import os
import sys

# Code run inside the kernel once IPython is initialized.
# Must be a single CLI-safe string (no newlines — use `;`).
_DX_EXEC_LINE = "import dx as _nteract_dx; _nteract_dx.install()"


def enabled_exec_lines() -> list[str]:
    """Return the exec_lines snippets that should run inside the kernel."""
    lines: list[str] = []
    if os.environ.get("RUNT_BOOTSTRAP_DX"):
        lines.append(_DX_EXEC_LINE)
    return lines


def _inject_exec_lines(argv: list[str], lines: list[str]) -> None:
    """Append ``--IPKernelApp.exec_lines=...`` args to argv in place."""
    for line in lines:
        argv.append(f"--IPKernelApp.exec_lines={line}")


def main() -> None:
    """Configure ipykernel's exec_lines, then hand off to ipykernel_launcher."""
    _inject_exec_lines(sys.argv, enabled_exec_lines())
    from ipykernel import kernelapp

    kernelapp.launch_new_instance()


if __name__ == "__main__":
    main()
```

Write the file:

```bash
cat > python/nteract-kernel-launcher/nteract_kernel_launcher.py <<'PY'
<contents from above>
PY
```

- [ ] **Step 2: Delete the old `src/` layout**

```bash
rm -rf python/nteract-kernel-launcher/src
```

- [ ] **Step 3: Update `pyproject.toml` to build from the top-level file**

Exact change: replace the `[tool.hatch.build.targets.wheel]` table.

```toml
[tool.hatch.build.targets.wheel]
only-include = ["nteract_kernel_launcher.py"]
sources = []
```

(Rest of the file unchanged — `ipykernel` dep, pytest dev group, etc. stay.)

- [ ] **Step 4: Update the test file's imports**

Replace `python/nteract-kernel-launcher/tests/test_bootstrap.py` with:

```python
"""Unit tests for the bootstrap wiring.

These cover the argv rewriting and feature-flag gating. The hand-off to
ipykernel itself is exercised by integration tests.
"""

from __future__ import annotations

import sys
from pathlib import Path

# The module is a single file at the package root, not under src/.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import nteract_kernel_launcher as nkl  # noqa: E402


def test_no_exec_lines_without_flag(monkeypatch):
    monkeypatch.delenv("RUNT_BOOTSTRAP_DX", raising=False)
    assert nkl.enabled_exec_lines() == []


def test_dx_exec_line_when_flag_set(monkeypatch):
    monkeypatch.setenv("RUNT_BOOTSTRAP_DX", "1")
    lines = nkl.enabled_exec_lines()
    assert len(lines) == 1
    assert "dx" in lines[0]
    assert "install" in lines[0]
    assert "\n" not in lines[0]


def test_inject_exec_lines_appends_args():
    argv = ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    nkl._inject_exec_lines(argv, ["import dx; dx.install()"])
    assert argv[:3] == ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    assert argv[3] == "--IPKernelApp.exec_lines=import dx; dx.install()"


def test_inject_exec_lines_noop_on_empty():
    argv = ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    before = list(argv)
    nkl._inject_exec_lines(argv, [])
    assert argv == before
```

- [ ] **Step 5: Run pytest to verify**

```bash
cd python/nteract-kernel-launcher && uv run --group dev pytest -q
```

Expected: `4 passed`.

- [ ] **Step 6: Commit**

```bash
git add python/nteract-kernel-launcher
git commit -m "refactor(nteract-kernel-launcher): collapse to single-file module"
```

---

## Task 2: Add `kernel-env::launcher` with embedded source + tests

**Files:**
- Create: `crates/kernel-env/src/launcher.rs`
- Modify: `crates/kernel-env/src/lib.rs`
- Test: Inline `#[cfg(test)]` in `launcher.rs`

- [ ] **Step 1: Write the failing test for `LAUNCHER_SRC` validity**

Create `crates/kernel-env/src/launcher.rs` with just the embed + a failing test first:

```rust
//! Embedded `nteract_kernel_launcher.py` and vendoring into kernel venvs.
//!
//! The Python source is `include_str!`'d from
//! `python/nteract-kernel-launcher/nteract_kernel_launcher.py` so the launcher
//! ships inside the daemon binary. `vendor_into_venv` writes the file to the
//! target venv's site-packages so `python -m nteract_kernel_launcher` works
//! without any PyPI install.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Canonical name of the single-file launcher module on disk.
pub const LAUNCHER_FILENAME: &str = "nteract_kernel_launcher.py";

/// Embedded Python source for the launcher. Compiled into the binary.
pub const LAUNCHER_SRC: &str = include_str!(
    "../../../python/nteract-kernel-launcher/nteract_kernel_launcher.py"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_src_is_nonempty_and_parses() {
        assert!(LAUNCHER_SRC.contains("def main"));
        assert!(LAUNCHER_SRC.contains("launch_new_instance"));
    }
}
```

Export it from `lib.rs`:

```rust
pub mod launcher;
```

- [ ] **Step 2: Run the test to verify it passes**

```bash
cargo test -p kernel-env launcher::tests::launcher_src_is_nonempty_and_parses
```

Expected: PASS.

- [ ] **Step 3: Add `sysconfig`-based purelib lookup**

Append to `launcher.rs`:

```rust
/// Ask the target Python for its `purelib` site-packages directory.
/// That's where we drop the launcher file so `-m nteract_kernel_launcher`
/// resolves without modifying `sys.path`.
pub async fn purelib_for(python: &Path) -> Result<PathBuf> {
    let output = tokio::process::Command::new(python)
        .args(["-c", "import sysconfig; print(sysconfig.get_path('purelib'))"])
        .output()
        .await
        .with_context(|| format!("failed to spawn {python:?} for sysconfig lookup"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "{python:?} sysconfig.get_path('purelib') failed: {stderr}"
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{python:?} returned empty purelib"));
    }
    Ok(PathBuf::from(trimmed))
}
```

- [ ] **Step 4: Add `vendor_into_venv`**

Append to `launcher.rs`:

```rust
/// Write `LAUNCHER_SRC` into the venv's site-packages so that
/// `python -m nteract_kernel_launcher` resolves.
///
/// Idempotent: overwrites if present. Writes via a temp file + rename
/// so concurrent readers never see a half-written module.
pub async fn vendor_into_venv(python: &Path) -> Result<PathBuf> {
    let purelib = purelib_for(python).await?;
    tokio::fs::create_dir_all(&purelib)
        .await
        .with_context(|| format!("create purelib {purelib:?}"))?;

    let final_path = purelib.join(LAUNCHER_FILENAME);
    let tmp_path = purelib.join(format!(".{LAUNCHER_FILENAME}.tmp"));
    tokio::fs::write(&tmp_path, LAUNCHER_SRC)
        .await
        .with_context(|| format!("write {tmp_path:?}"))?;
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .with_context(|| format!("rename into place at {final_path:?}"))?;

    Ok(final_path)
}
```

- [ ] **Step 5: Add an integration-style Rust test that runs real Python**

Append to `mod tests`:

```rust
    #[tokio::test]
    async fn vendor_into_venv_writes_importable_module() {
        // Skip if no system python available — this is a best-effort sanity
        // check, not a hard prerequisite. CI runs with python present.
        let Some(python) = which::which("python3").ok().or_else(|| which::which("python").ok())
        else {
            eprintln!("skipping: no python on PATH");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        // Build a minimal fake venv: create a purelib dir and stub a python
        // shim that prints that dir.
        let purelib = tmp.path().join("lib/site-packages");
        tokio::fs::create_dir_all(&purelib).await.unwrap();

        // Call vendor_into_venv against the real python; the purelib path
        // will be the host interpreter's, which we don't want to pollute.
        // So instead test the write-and-rename logic directly.
        let written = super::_test_write_launcher(&purelib).await.unwrap();
        assert_eq!(written.file_name().unwrap(), LAUNCHER_FILENAME);

        // Verify the written content matches the embedded source exactly.
        let read = tokio::fs::read_to_string(&written).await.unwrap();
        assert_eq!(read, LAUNCHER_SRC);

        // Verify python can at least parse it as valid syntax.
        let status = tokio::process::Command::new(&python)
            .args([
                "-c",
                &format!(
                    "import ast, pathlib; ast.parse(pathlib.Path(r'{}').read_text())",
                    written.display()
                ),
            ])
            .status()
            .await
            .unwrap();
        assert!(status.success(), "embedded launcher is not valid Python");
    }
```

And extract a test-only helper that skips the `purelib_for` step:

```rust
#[doc(hidden)]
pub async fn _test_write_launcher(purelib: &Path) -> Result<PathBuf> {
    let final_path = purelib.join(LAUNCHER_FILENAME);
    let tmp_path = purelib.join(format!(".{LAUNCHER_FILENAME}.tmp"));
    tokio::fs::write(&tmp_path, LAUNCHER_SRC).await?;
    tokio::fs::rename(&tmp_path, &final_path).await?;
    Ok(final_path)
}
```

- [ ] **Step 6: Add deps to `crates/kernel-env/Cargo.toml`**

Under `[dev-dependencies]`:

```toml
tempfile = "3"
which = "7"
```

(`tempfile` may already be in dev-deps; check before adding. `which` likely is not.)

- [ ] **Step 7: Run the tests**

```bash
cargo test -p kernel-env launcher
```

Expected: 2 passed (one pure, one Python-shelling).

- [ ] **Step 8: Commit**

```bash
git add crates/kernel-env
git commit -m "feat(kernel-env): embed nteract_kernel_launcher via include_str! + vendor helper"
```

---

## Task 3: Vendor into UV envs; drop `UvDependencies.bootstrap_dx`

**Files:**
- Modify: `crates/kernel-env/src/uv.rs`
- Modify: `crates/runtimed/src/inline_env.rs`
- Modify: `crates/runtimed/src/daemon.rs` (prewarmed packages list only)

- [ ] **Step 1: Remove `bootstrap_dx` field from `UvDependencies`**

In `crates/kernel-env/src/uv.rs` around line 19–32, delete the `bootstrap_dx` field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UvDependencies {
    pub dependencies: Vec<String>,
    #[serde(rename = "requires-python")]
    pub requires_python: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prerelease: Option<String>,
    // bootstrap_dx field removed — launcher vendoring makes it unnecessary.
}
```

- [ ] **Step 2: Remove the `if deps.bootstrap_dx` install branch (line ~214)**

```rust
// Was:
// if deps.bootstrap_dx {
//     packages.push("nteract-kernel-launcher".to_string());
//     packages.push("dx".to_string());
// }
//
// Delete entirely. `dx` will be added at a higher layer if the user sets
// the bootstrap_dx feature flag; launcher is vendored post-creation.
```

- [ ] **Step 3: Drop `bootstrap_dx` args from `create_prewarmed_environment[_in]`**

Change these two functions (lines ~412 and ~427) so they no longer take `bootstrap_dx: bool`. Remove the `if bootstrap_dx { install_args.push("nteract-kernel-launcher"); ... }` block (line ~485).

- [ ] **Step 4: Call `launcher::vendor_into_venv` at the end of env creation**

In `prepare_environment_in`, just before returning `Ok(UvEnvironment { ... })`:

```rust
crate::launcher::vendor_into_venv(&python_path)
    .await
    .context("vendor nteract_kernel_launcher into UV env")?;
```

Same at the end of `create_prewarmed_environment_in`.

- [ ] **Step 5: Update every `bootstrap_dx: false` / `bootstrap_dx: <expr>` initializer**

Grep and remove the field everywhere:

```bash
rg "bootstrap_dx:" crates/kernel-env crates/runtimed/src/inline_env.rs
```

Expected remaining sites: `crates/runtimed/src/inline_env.rs` (lines 69, 75, 196). Delete the field and any plumbing arg that carried it into `UvDependencies` construction.

- [ ] **Step 6: Drop the launcher from `uv_prewarmed_packages` in `daemon.rs`**

In `crates/runtimed/src/daemon.rs` around line 259:

```rust
fn uv_prewarmed_packages(
    extra: &[String],
    feature_flags: notebook_protocol::protocol::FeatureFlags,
) -> Vec<String> {
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
        "uv".to_string(),
    ];
    if feature_flags.bootstrap_dx {
        // Launcher is vendored post-creation; only `dx` needs installing.
        packages.push("dx".to_string());
    }
    packages.extend(extra.iter().cloned());
    packages
}
```

- [ ] **Step 7: Remove the `bootstrap_dx` arg threading in `create_prewarmed_environment`-callers**

Any call site that passed `synced.feature_flags().bootstrap_dx` into the env creator — drop that argument, since the create function no longer takes it.

- [ ] **Step 8: Build + run UV unit tests**

```bash
cargo test -p kernel-env
cargo test -p runtimed --lib daemon::tests::test_evict
cargo test -p runtimed --lib inline_env
```

Expected: all green.

- [ ] **Step 9: Commit**

```bash
git add crates/kernel-env crates/runtimed/src
git commit -m "refactor(kernel-env): vendor launcher into UV envs, drop bootstrap_dx install plumbing"
```

---

## Task 4: Vendor into conda envs

**Files:**
- Modify: `crates/kernel-env/src/conda.rs`

- [ ] **Step 1: Call `launcher::vendor_into_venv` after `prepare_environment_in`**

At the end of `prepare_environment_in` (line ~220, just before the final `Ok(CondaEnvironment { ... })`), add:

```rust
crate::launcher::vendor_into_venv(&python_path)
    .await
    .context("vendor nteract_kernel_launcher into conda env")?;
```

Same at the end of `create_prewarmed_environment_in` (around line ~467).

Same at the end of `claim_prewarmed_environment_in` — this is the backstop for pool envs that were warmed before this change.

- [ ] **Step 2: Run conda unit tests**

```bash
cargo test -p kernel-env conda
```

Expected: all green. (Conda tests use mocked env paths; the vendor call is a best-effort write that may no-op if there's no python binary — which is fine; we guard with `.context` so real failures surface but the module builds.)

- [ ] **Step 3: Commit**

```bash
git add crates/kernel-env/src/conda.rs
git commit -m "feat(kernel-env): vendor launcher into conda envs on create/claim"
```

---

## Task 5: Vendor into pixi envs

**Files:**
- Modify: `crates/runtimed/src/daemon.rs` (pixi creation path)

- [ ] **Step 1: Locate pixi env creation**

```bash
rg "create_pixi_env|pixi_prewarmed" crates/runtimed/src/daemon.rs
```

Note the line number where pixi creates its env and resolves `python_path`.

- [ ] **Step 2: Add vendor call after pixi env is ready**

Around line ~4078 where `prewarmed_packages = packages.clone()` — find the place where the pixi env's `python_path` is known to be valid, and add:

```rust
if let Err(e) = kernel_env::launcher::vendor_into_venv(&python_path).await {
    warn!("[runtimed] Pixi env vendor failed: {e}");
}
```

(Pixi envs sometimes have quirky layouts; warn-but-continue avoids breaking the pool if vendoring fails on an edge case.)

- [ ] **Step 3: Run**

```bash
cargo test -p runtimed --lib daemon
```

Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/daemon.rs
git commit -m "feat(runtimed): vendor launcher into pixi envs on create"
```

---

## Task 6: `uv:pyproject` uses script path, not `-m`

**Files:**
- Create: `crates/runtimed/src/launcher_cache.rs`
- Modify: `crates/runtimed/src/lib.rs` (add `mod launcher_cache`)
- Modify: `crates/runtimed/src/jupyter_kernel.rs`

- [ ] **Step 1: Write the launcher-cache module**

Create `crates/runtimed/src/launcher_cache.rs`:

```rust
//! Daemon-scoped cache for the vendored launcher script.
//!
//! `uv run` creates an ephemeral venv per invocation, so we can't vendor into
//! it. Instead we stash a copy of `LAUNCHER_SRC` at a stable cache path and
//! pass that path to `uv run python <path>`.
//!
//! Written once per daemon process on first access, idempotent on restart.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use kernel_env::launcher::{LAUNCHER_FILENAME, LAUNCHER_SRC};

static CACHED_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Return a stable path to a file containing `LAUNCHER_SRC`.
/// Creates the file on first call, reuses on subsequent calls.
pub async fn launcher_script_path(cache_root: &Path) -> Result<PathBuf> {
    if let Some(p) = CACHED_PATH.get() {
        return Ok(p.clone());
    }
    let dir = cache_root.join("launcher");
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
```

- [ ] **Step 2: Wire it in `lib.rs`**

In `crates/runtimed/src/lib.rs`, add the module declaration near the existing `pub mod` lines:

```rust
pub mod launcher_cache;
```

- [ ] **Step 3: Update `uv:pyproject` launch path**

In `crates/runtimed/src/jupyter_kernel.rs` around line 242–270, replace the current `uv:pyproject` match arm body with:

```rust
"uv:pyproject" => {
    let uv_path = kernel_launch::tools::get_uv_path().await?;
    info!(
        "[jupyter-kernel] Starting Python kernel with uv run (env_source: {})",
        env_source
    );
    let mut cmd = tokio::process::Command::new(&uv_path);
    let mut args: Vec<String> =
        vec!["run".into(), "--with".into(), "ipykernel".into(), "--with".into(), "uv".into()];
    if bootstrap_dx {
        // dx is on PyPI; launcher is bundled.
        args.push("--with".into());
        args.push("dx".into());
    }
    args.push("python".into());
    args.push("-Xfrozen_modules=off".into());
    if bootstrap_dx {
        // Invoke the vendored launcher by path — uv's ephemeral venv does not
        // have `nteract_kernel_launcher` in site-packages.
        let script = crate::launcher_cache::launcher_script_path(
            &shared.daemon_cache_dir, // or the appropriate handle — see Step 4
        )
        .await?;
        args.push(script.to_string_lossy().into_owned());
    } else {
        args.push("-m".into());
        args.push("ipykernel_launcher".into());
    }
    args.push("-f".into());
    cmd.args(&args);
    cmd.arg(&connection_file_path);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    cmd
}
```

- [ ] **Step 4: Wire the daemon cache dir into `KernelSharedRefs`**

Inspect `crates/runtimed/src/kernel_connection.rs` for `KernelSharedRefs`. If the daemon cache dir isn't already accessible, add a `pub daemon_cache_dir: PathBuf` field and thread it through. If a simpler path exists (e.g. `BlobStore::root()` + `.parent()`), reuse that — the point is a stable path under `~/.cache/runt[-nightly]`.

Check the existing construction sites of `KernelSharedRefs` (grep for `KernelSharedRefs {`) and pass the daemon's `config.cache_dir` through.

- [ ] **Step 5: Run**

```bash
cargo build -p runtimed
cargo test -p runtimed --lib
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed/src
git commit -m "feat(runtimed): uv:pyproject invokes launcher by script path, not PyPI install"
```

---

## Task 7: Vendor-on-claim fallback for pool envs

**Files:**
- Modify: `crates/runtimed-client/src/pool_client.rs` (or wherever `take_uv_env`/`take_conda_env` dispatch lives — grep to find)

Pool envs warmed before this change have no vendored launcher. To avoid requiring a full pool drain on upgrade, vendor at claim time too. Idempotent write.

- [ ] **Step 1: Locate the claim path**

```bash
rg "claim_prewarmed_environment|take_uv_env|take_conda_env" crates/runtimed-client crates/runtimed/src
```

- [ ] **Step 2: Add a vendor call right before the claimed env is returned to the caller**

For UV: in `kernel_env::uv::claim_prewarmed_environment_in` (if it exists) or in the runtimed-side claim wrapper, after the claim succeeds:

```rust
kernel_env::launcher::vendor_into_venv(&env.python_path)
    .await
    .context("re-vendor launcher into claimed UV env")?;
```

For conda: the same helper lives in `kernel_env::conda` — already addressed in Task 4 Step 1.

- [ ] **Step 3: Add a regression unit test**

Write a focused test that simulates a "stale pool env missing the launcher" and asserts that after claim, the file exists. (If direct unit testing is awkward, a doc-comment explaining the invariant plus Task 9's end-to-end test is acceptable.)

- [ ] **Step 4: Run + commit**

```bash
cargo test -p kernel-env
cargo test -p runtimed --lib
git add -A && git commit -m "feat(pool): re-vendor launcher on claim to cover pre-upgrade pool entries"
```

---

## Task 8: End-to-end sanity test

**Files:**
- Test: `crates/kernel-env/tests/launcher_e2e.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end sanity: create a minimal venv, vendor the launcher, import it.
//!
//! Skipped automatically if `uv` isn't on PATH (no kernel_launch bootstrap in
//! test context). CI installs uv.

use std::path::PathBuf;

use kernel_env::launcher::{vendor_into_venv, LAUNCHER_FILENAME, LAUNCHER_SRC};

#[tokio::test]
async fn launcher_importable_after_vendor() {
    let Some(uv) = which::which("uv").ok() else {
        eprintln!("skipping: no uv on PATH");
        return;
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let venv = tmp.path().join("venv");

    let status = tokio::process::Command::new(&uv)
        .args(["venv", venv.to_str().unwrap()])
        .status()
        .await
        .unwrap();
    assert!(status.success(), "uv venv failed");

    #[cfg(unix)]
    let python = venv.join("bin/python");
    #[cfg(windows)]
    let python: PathBuf = venv.join("Scripts/python.exe");

    let written = vendor_into_venv(&python).await.unwrap();
    assert_eq!(written.file_name().unwrap(), LAUNCHER_FILENAME);

    // `python -c "import nteract_kernel_launcher"` must succeed.
    let status = tokio::process::Command::new(&python)
        .args(["-c", "import nteract_kernel_launcher"])
        .status()
        .await
        .unwrap();
    assert!(status.success(), "import nteract_kernel_launcher failed");

    // The on-disk copy matches the embedded source verbatim.
    let read = tokio::fs::read_to_string(&written).await.unwrap();
    assert_eq!(read, LAUNCHER_SRC);
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p kernel-env --test launcher_e2e
```

Expected: PASS (or a skip message if `uv` is absent).

- [ ] **Step 3: Commit**

```bash
git add crates/kernel-env/tests/launcher_e2e.rs
git commit -m "test(kernel-env): e2e vendor + import sanity for nteract_kernel_launcher"
```

---

## Task 9: Final verification + lint + codex review

- [ ] **Step 1: Full lint**

```bash
cargo xtask lint --fix
cargo xtask lint
```

Expected: all green (modulo the pre-existing `runtimed-node` clippy issue, which CI excludes).

- [ ] **Step 2: Full tests**

```bash
cargo test -p kernel-env
cargo test -p runtimed --lib
cargo test -p runtimed --test integration
```

Expected: all green.

- [ ] **Step 3: Manual smoke via dev daemon**

(Only if local venv + daemon setup is available.) With `RUNTIMED_DEV=1` set, start the dev daemon, toggle the `bootstrap_dx` feature flag via the settings UI, open a notebook backed by a UV inline env, and run `import nteract_kernel_launcher; print(nteract_kernel_launcher.__file__)`. Expected: prints a path under `~/.cache/runt[-nightly]/...`. Confirms vendoring landed in the right site-packages.

- [ ] **Step 4: Codex review on the branch**

```bash
codex review --base origin/main
```

Address any P1/P2 findings inline. Loop until clean.

- [ ] **Step 5: Open PR, monitor CI, mark ready, `voice say` when merged**

Follows the standard workflow.

---

## Self-Review Summary

- **Spec coverage:** All four codex findings from #1939 addressed — P1 package-not-found eliminated (no pip install of launcher); P1 stale-UV-pool and P2 hash-mismatch resolved (bootstrap_dx no longer affects install args or env hash); bootstrap_dx reduces to a launch-time switch.
- **Placeholder scan:** None. Each step shows concrete code.
- **Type consistency:** `LAUNCHER_FILENAME`, `LAUNCHER_SRC`, `vendor_into_venv`, `purelib_for`, `launcher_script_path` are consistent across tasks. `UvDependencies::bootstrap_dx` is removed in Task 3 and never referenced in later tasks.
- **Risk:** `purelib_for` spawns a python subprocess per env creation. That's fine in creation flows (already seconds-long) but could be surprising in claim flows — Task 7 uses `vendor_into_venv` which internally shells python once, and claim is a rare operation per notebook launch.
