//! Embedded `nteract_kernel_launcher` package and vendoring into kernel venvs.
//!
//! The Python sources are `include_str!`'d from
//! `python/nteract-kernel-launcher/nteract_kernel_launcher/` so the launcher
//! ships inside the daemon binary. `vendor_into_venv` writes the package
//! directory into the target venv's site-packages so `python -m
//! nteract_kernel_launcher` works without any PyPI install.
//!
//! The single-file legacy layout (a flat `nteract_kernel_launcher.py` sitting
//! next to site-packages) was replaced by a proper package in 0.2.0 so we
//! could vendor the IPython extension plus the dx internals (formatters,
//! buffer hooks, summaries) without a PyPI dependency. Vendor removes any
//! stale single-file module before writing the package.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Canonical name of the launcher package directory inside site-packages.
pub const LAUNCHER_PKG: &str = "nteract_kernel_launcher";

/// Legacy single-file module name. Removed on vendor if found so the old
/// module doesn't shadow the new package.
pub const LAUNCHER_LEGACY_FILE: &str = "nteract_kernel_launcher.py";

/// The package's Python sources, paired with their relative path inside
/// the package directory. Order is irrelevant — each file is written
/// independently via write-and-rename.
pub const LAUNCHER_FILES: &[(&str, &str)] = &[
    (
        "__init__.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/__init__.py"),
    ),
    (
        "__main__.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/__main__.py"),
    ),
    (
        "app.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/app.py"),
    ),
    (
        "_bootstrap.py",
        include_str!(
            "../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_bootstrap.py"
        ),
    ),
    (
        "_buffer_hook.py",
        include_str!(
            "../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_buffer_hook.py"
        ),
    ),
    (
        "_env.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_env.py"),
    ),
    (
        "_format.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_format.py"),
    ),
    (
        "_refs.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_refs.py"),
    ),
    (
        "_summary.py",
        include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher/_summary.py"),
    ),
];

/// Ask the target Python for its `purelib` site-packages directory.
/// That's where we drop the launcher package so `-m nteract_kernel_launcher`
/// resolves without modifying `sys.path`.
pub async fn purelib_for(python: &Path) -> Result<PathBuf> {
    let output = tokio::process::Command::new(python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
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

/// Per-file per-call unique temp path for write-and-rename.
///
/// A fixed tmp filename races when two vendors target the same
/// site-packages directory. `pid + nanos + basename` keeps each caller's
/// tmp unique.
fn unique_tmp_path(dir: &Path, basename: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(".{basename}.tmp.{pid}.{nanos}"))
}

/// Remove any pre-0.2.0 single-file module so it doesn't shadow the
/// package we're about to write. Missing file is a no-op.
async fn remove_legacy_single_file(purelib: &Path) -> Result<()> {
    let legacy = purelib.join(LAUNCHER_LEGACY_FILE);
    match tokio::fs::remove_file(&legacy).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove legacy module {legacy:?}")),
    }
}

/// Write every embedded launcher file into `pkg_dir` atomically.
///
/// Each file goes to a unique temp path and then renames into place, so
/// a concurrent reader importing from the directory never sees a
/// half-written module. Exposed so the daemon's launcher cache
/// (`runtimed::launcher_cache`) reuses the same atomic-write pattern
/// the per-venv vendoring path relies on.
pub async fn write_package_files(pkg_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(pkg_dir)
        .await
        .with_context(|| format!("create package dir {pkg_dir:?}"))?;

    for (relpath, contents) in LAUNCHER_FILES {
        let final_path = pkg_dir.join(relpath);
        let tmp_path = unique_tmp_path(pkg_dir, relpath);
        tokio::fs::write(&tmp_path, contents)
            .await
            .with_context(|| format!("write {tmp_path:?}"))?;
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .with_context(|| format!("rename into place at {final_path:?}"))?;
    }
    Ok(())
}

/// Write the `nteract_kernel_launcher` package into the venv's
/// site-packages so that `python -m nteract_kernel_launcher` resolves.
///
/// Idempotent: overwrites existing files. Each file is written via
/// temp + rename so concurrent readers never see a half-written module.
/// Temp filenames are unique per call so concurrent vendors into the
/// same site-packages don't race on the rename.
///
/// Also removes any pre-0.2.0 flat `nteract_kernel_launcher.py` so it
/// can't shadow the package.
pub async fn vendor_into_venv(python: &Path) -> Result<PathBuf> {
    let purelib = purelib_for(python).await?;
    tokio::fs::create_dir_all(&purelib)
        .await
        .with_context(|| format!("create purelib {purelib:?}"))?;

    remove_legacy_single_file(&purelib).await?;

    let pkg_dir = purelib.join(LAUNCHER_PKG);
    write_package_files(&pkg_dir).await?;
    Ok(pkg_dir)
}

/// Test-only helper: write the package to a caller-provided purelib dir
/// without calling into Python to resolve it. Exposed so unit tests can
/// exercise the write-and-rename logic without polluting the host
/// interpreter's real site-packages.
#[doc(hidden)]
pub async fn _test_write_launcher(purelib: &Path) -> Result<PathBuf> {
    remove_legacy_single_file(purelib).await?;
    let pkg_dir = purelib.join(LAUNCHER_PKG);
    write_package_files(&pkg_dir).await?;
    Ok(pkg_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_sources_nonempty() {
        assert!(!LAUNCHER_FILES.is_empty());
        // Every entry must have non-empty contents — a missing include_str!
        // would fail at compile time, but a wrong path with an empty file
        // would silently ship broken.
        for (name, contents) in LAUNCHER_FILES {
            assert!(!contents.trim().is_empty(), "empty launcher file: {name}");
        }
    }

    #[test]
    fn launcher_exposes_entry_point() {
        let init = LAUNCHER_FILES
            .iter()
            .find(|(n, _)| *n == "__init__.py")
            .expect("__init__.py must be included");
        assert!(init.1.contains("def main"));
        assert!(init.1.contains("NteractKernelApp"));
    }

    #[test]
    fn launcher_ships_bootstrap_extension() {
        let boot = LAUNCHER_FILES
            .iter()
            .find(|(n, _)| *n == "_bootstrap.py")
            .expect("_bootstrap.py must be included");
        assert!(boot.1.contains("def load_ipython_extension"));
    }

    #[tokio::test]
    async fn vendor_writes_importable_package() {
        // Skip if no system python available — this is a best-effort sanity
        // check, not a hard prerequisite. CI runs with python present.
        let Some(python) = which::which("python3")
            .ok()
            .or_else(|| which::which("python").ok())
        else {
            eprintln!("skipping: no python on PATH");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let purelib = tmp.path().join("lib/site-packages");
        tokio::fs::create_dir_all(&purelib).await.unwrap();

        let written = super::_test_write_launcher(&purelib).await.unwrap();
        assert_eq!(written.file_name().unwrap(), LAUNCHER_PKG);

        // All package files must land on disk with the embedded contents.
        for (relpath, contents) in LAUNCHER_FILES {
            let path = written.join(relpath);
            let read = tokio::fs::read_to_string(&path).await.unwrap();
            assert_eq!(&read, *contents, "mismatch in {relpath}");
        }

        // Verify python can parse every vendored file as valid syntax.
        for (relpath, _) in LAUNCHER_FILES {
            let path = written.join(relpath);
            let status = tokio::process::Command::new(&python)
                .args([
                    "-c",
                    &format!(
                        "import ast, pathlib; ast.parse(pathlib.Path(r'{}').read_text())",
                        path.display()
                    ),
                ])
                .status()
                .await
                .unwrap();
            assert!(status.success(), "invalid Python in vendored {relpath}");
        }
    }

    #[tokio::test]
    async fn vendor_removes_legacy_single_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let purelib = tmp.path().join("lib/site-packages");
        tokio::fs::create_dir_all(&purelib).await.unwrap();

        // Seed the legacy file — a stale single-module from 0.1.x.
        let legacy = purelib.join(LAUNCHER_LEGACY_FILE);
        tokio::fs::write(&legacy, "# stale single-file launcher\n")
            .await
            .unwrap();
        assert!(legacy.exists());

        let written = super::_test_write_launcher(&purelib).await.unwrap();
        assert!(!legacy.exists(), "legacy single-file not cleaned up");
        assert!(written.is_dir());
        assert!(written.join("__init__.py").exists());
    }

    #[tokio::test]
    async fn concurrent_writes_dont_race() {
        // Two concurrent writes into the same purelib must both succeed.
        // A fixed `.tmp` filename would make the second rename fail with
        // ENOENT once the first finishes — per-call unique tmps avoid that.
        let tmp = tempfile::TempDir::new().unwrap();
        let purelib = tmp.path().join("lib/site-packages");
        tokio::fs::create_dir_all(&purelib).await.unwrap();

        let p1 = purelib.clone();
        let p2 = purelib.clone();
        let (r1, r2) = tokio::join!(
            super::_test_write_launcher(&p1),
            super::_test_write_launcher(&p2),
        );

        assert!(r1.is_ok(), "first concurrent write failed: {:?}", r1);
        assert!(r2.is_ok(), "second concurrent write failed: {:?}", r2);

        let pkg_dir = purelib.join(LAUNCHER_PKG);
        assert!(pkg_dir.is_dir(), "package dir not present after race");
        for (relpath, contents) in LAUNCHER_FILES {
            let read = tokio::fs::read_to_string(pkg_dir.join(relpath))
                .await
                .unwrap();
            assert_eq!(&read, *contents, "mismatch in {relpath}");
        }
    }
}
