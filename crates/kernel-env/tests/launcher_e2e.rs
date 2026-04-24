// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end sanity: create a minimal venv, vendor the launcher package,
//! import it.
//!
//! Skipped automatically if `uv` isn't on PATH (no kernel_launch bootstrap in
//! test context). CI installs uv.

#[cfg(windows)]
use std::path::PathBuf;

use kernel_env::launcher::{vendor_into_venv, LAUNCHER_FILES, LAUNCHER_PKG};

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
    assert_eq!(written.file_name().unwrap(), LAUNCHER_PKG);
    assert!(written.is_dir(), "launcher vendored as package dir");

    // `python -c "import nteract_kernel_launcher"` must succeed. The bootstrap
    // extension has runtime imports (ipykernel, traitlets) that a bare venv
    // won't have, so we only exercise the top-level __init__.py here.
    // Importing __init__.py pulls in app.py, which does need ipykernel —
    // install it into the venv first.
    let status = tokio::process::Command::new(&uv)
        .args([
            "pip",
            "install",
            "--python",
            python.to_str().unwrap(),
            "ipykernel",
        ])
        .status()
        .await
        .unwrap();
    assert!(status.success(), "uv pip install ipykernel failed");

    let status = tokio::process::Command::new(&python)
        .args(["-c", "import nteract_kernel_launcher"])
        .status()
        .await
        .unwrap();
    assert!(status.success(), "import nteract_kernel_launcher failed");

    // Every embedded file landed in the package dir with matching contents.
    for (relpath, contents) in LAUNCHER_FILES {
        let path = written.join(relpath);
        let read = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(&read, *contents, "mismatch in {relpath}");
    }
}
