// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end sanity: create a minimal venv, vendor the launcher, import it.
//!
//! Skipped automatically if `uv` isn't on PATH (no kernel_launch bootstrap in
//! test context). CI installs uv.

#[cfg(windows)]
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
