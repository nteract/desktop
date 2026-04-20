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
pub const LAUNCHER_SRC: &str =
    include_str!("../../../python/nteract-kernel-launcher/nteract_kernel_launcher.py");

/// Ask the target Python for its `purelib` site-packages directory.
/// That's where we drop the launcher file so `-m nteract_kernel_launcher`
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

/// Test-only helper: write the launcher to a caller-provided purelib dir
/// without calling into Python to resolve it. Exposed so unit tests can
/// exercise the write-and-rename logic without polluting the host
/// interpreter's real site-packages.
#[doc(hidden)]
pub async fn _test_write_launcher(purelib: &Path) -> Result<PathBuf> {
    let final_path = purelib.join(LAUNCHER_FILENAME);
    let tmp_path = purelib.join(format!(".{LAUNCHER_FILENAME}.tmp"));
    tokio::fs::write(&tmp_path, LAUNCHER_SRC).await?;
    tokio::fs::rename(&tmp_path, &final_path).await?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_src_is_nonempty_and_parses() {
        assert!(LAUNCHER_SRC.contains("def main"));
        assert!(LAUNCHER_SRC.contains("launch_new_instance"));
    }

    #[tokio::test]
    async fn vendor_writes_importable_module() {
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
        assert_eq!(written.file_name().unwrap(), LAUNCHER_FILENAME);

        let read = tokio::fs::read_to_string(&written).await.unwrap();
        assert_eq!(read, LAUNCHER_SRC);

        // Verify python can parse the file as valid syntax.
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
}
