//! Python environment management (UV + Conda) with progress reporting.
//!
//! This crate provides the core environment creation, caching, and prewarming
//! logic used by both the notebook app and the runtimed daemon. It includes:
//!
//! - A progress reporting trait for environment lifecycle events
//! - UV virtual environment creation via `uv`
//! - Conda environment creation via `rattler`
//! - Hash-based caching for instant reuse
//! - Prewarming support for fast kernel startup
//!
//! # Progress Reporting
//!
//! All environment operations accept a [`ProgressHandler`] to report phases
//! like fetching repodata, solving, downloading, and linking. Consumers
//! implement this trait to route progress to their UI (Tauri events, daemon
//! broadcast channel, logs, etc.).
//!
//! ```ignore
//! use kernel_env::progress::{LogHandler, ProgressHandler};
//!
//! // Log-only progress
//! let handler = LogHandler;
//! kernel_env::conda::prepare_environment(&deps, &handler).await?;
//! ```

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

#[cfg(feature = "runtime")]
pub mod conda;
#[cfg(feature = "runtime")]
pub mod gc;
pub mod launcher;
#[cfg(feature = "runtime")]
pub mod lock;
#[cfg(feature = "runtime")]
pub mod pixi;
pub mod progress;
#[cfg(feature = "runtime")]
pub mod repodata;
#[cfg(feature = "runtime")]
pub mod uv;
pub mod warmup;

// Re-export key types
#[cfg(feature = "runtime")]
pub use conda::{CondaDependencies, CondaEnvironment, CONDA_BASE_PACKAGES};
pub use progress::{EnvProgressPhase, LogHandler, ProgressHandler};
#[cfg(feature = "runtime")]
pub use uv::{UvDependencies, UvEnvironment, UV_BASE_PACKAGES};

/// Return the subset of `installed` that isn't in `base`, preserving input order.
///
/// Used by the unified env resolution design to derive the user-level dep set
/// from a freshly-claimed pool env's full install list. Pool warmers install
/// `[ipykernel, ipywidgets, …, <user_defaults…>]`; at capture time we strip
/// the known base set so the notebook's metadata carries only the user deps.
///
/// Comparison is exact-match on package name. If a name appears multiple times
/// in `installed`, every occurrence is dropped as long as it's in `base`.
#[cfg(feature = "runtime")]
pub fn strip_base(installed: &[String], base: &[&str]) -> Vec<String> {
    installed
        .iter()
        .filter(|pkg| !base.contains(&pkg.as_str()))
        .cloned()
        .collect()
}

/// Diagnostic result for checking whether a prepared Python environment can
/// import `ipykernel`.
#[cfg(feature = "runtime")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpykernelDiagnostic {
    Present {
        python_path: std::path::PathBuf,
        purelib: std::path::PathBuf,
    },
    Missing {
        python_path: std::path::PathBuf,
        purelib: std::path::PathBuf,
        import_error: Option<String>,
    },
    SitePackagesMismatch {
        python_path: std::path::PathBuf,
        purelib: std::path::PathBuf,
        import_error: Option<String>,
        candidates: Vec<std::path::PathBuf>,
    },
    InterpreterProbeFailed {
        python_path: std::path::PathBuf,
        message: String,
    },
}

#[cfg(feature = "runtime")]
impl IpykernelDiagnostic {
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

#[cfg(feature = "runtime")]
#[derive(Debug, serde::Deserialize)]
struct IpykernelProbe {
    purelib: String,
    import_ok: bool,
    error: Option<String>,
}

/// Check if a prepared UV venv or Conda env can import `ipykernel`.
///
/// Used by the inline/pep723 launch paths to detect missing `ipykernel` before
/// spawning the kernel. `prepare_environment_in` always adds `ipykernel` to
/// the install set, but cache hits and hand-edited venvs can route around
/// that, and the kernel then fails at spawn with a generic `ModuleNotFoundError`.
/// Gating here surfaces the typed `MissingIpykernel` reason so the UI can
/// render env-specific remediation.
///
/// Resolves site-packages by asking the interpreter itself
/// (`sysconfig.get_paths()['purelib']`) rather than scanning `lib/python*`
/// — `read_dir` order would pick an arbitrary Python-version directory on
/// envs with more than one (stale cache, interpreter upgrade), which can
/// false-negative a working env. The subprocess adds ~50ms; kernel launch
/// is already seconds.
///
#[cfg(feature = "runtime")]
pub fn diagnose_ipykernel(python_path: &std::path::Path) -> IpykernelDiagnostic {
    let probe = match probe_ipykernel(python_path) {
        Ok(probe) => probe,
        Err(message) => {
            return IpykernelDiagnostic::InterpreterProbeFailed {
                python_path: python_path.to_path_buf(),
                message,
            };
        }
    };

    let purelib = std::path::PathBuf::from(probe.purelib);
    if probe.import_ok {
        return IpykernelDiagnostic::Present {
            python_path: python_path.to_path_buf(),
            purelib,
        };
    }

    let candidates = sibling_site_packages_with_ipykernel(&purelib);
    if !candidates.is_empty() {
        return IpykernelDiagnostic::SitePackagesMismatch {
            python_path: python_path.to_path_buf(),
            purelib,
            import_error: probe.error,
            candidates,
        };
    }

    IpykernelDiagnostic::Missing {
        python_path: python_path.to_path_buf(),
        purelib,
        import_error: probe.error,
    }
}

/// Backward-compatible boolean wrapper for callers/tests that only need a gate.
#[cfg(feature = "runtime")]
pub fn venv_has_ipykernel(python_path: &std::path::Path) -> bool {
    diagnose_ipykernel(python_path).is_present()
}

#[cfg(feature = "runtime")]
fn probe_ipykernel(python_path: &std::path::Path) -> Result<IpykernelProbe, String> {
    let output = std::process::Command::new(python_path)
        .args([
            "-c",
            r#"import json, sysconfig
try:
    import ipykernel  # noqa: F401
    import_ok = True
    error = None
except Exception as exc:
    import_ok = False
    error = f"{type(exc).__name__}: {exc}"
print(json.dumps({
    "purelib": sysconfig.get_paths().get("purelib", ""),
    "import_ok": import_ok,
    "error": error,
}))"#,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("interpreter exited with status {}", output.status)
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let probe: IpykernelProbe =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("parse probe output: {e}"))?;
    if probe.purelib.trim().is_empty() {
        return Err("interpreter returned empty purelib".to_string());
    }
    Ok(probe)
}

#[cfg(feature = "runtime")]
fn site_packages_has_ipykernel(site_packages: &std::path::Path) -> bool {
    // Fast path: the importable package directory.
    if site_packages.join("ipykernel").is_dir() {
        return true;
    }
    // Fallback: dist-info metadata directory (covers odd install layouts
    // where the package is packaged differently, e.g. editable installs).
    let Ok(entries) = std::fs::read_dir(site_packages) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with("ipykernel-") && name_str.ends_with(".dist-info") {
            return true;
        }
    }
    false
}

#[cfg(feature = "runtime")]
fn sibling_site_packages_with_ipykernel(purelib: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(python_dir) = purelib.parent() else {
        return Vec::new();
    };
    let Some(lib_dir) = python_dir.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(lib_dir) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == python_dir || !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("python") {
            continue;
        }
        let site_packages = path.join("site-packages");
        if site_packages != purelib && site_packages_has_ipykernel(&site_packages) {
            candidates.push(site_packages);
        }
    }
    candidates.sort();
    candidates
}

#[cfg(all(test, feature = "runtime"))]
mod strip_base_tests {
    use super::*;

    #[test]
    fn strips_uv_base_leaves_user_defaults() {
        let installed: Vec<String> = [
            "ipykernel",
            "ipywidgets",
            "anywidget",
            "nbformat",
            "pyarrow>=14",
            "uv",
            "pandas",
            "numpy",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let result = strip_base(&installed, UV_BASE_PACKAGES);
        assert_eq!(result, vec!["pandas".to_string(), "numpy".to_string()]);
    }

    #[test]
    fn strips_conda_base_leaves_user_defaults() {
        let installed: Vec<String> = [
            "ipykernel",
            "ipywidgets",
            "anywidget",
            "nbformat",
            "pyarrow>=14",
            "scipy",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let result = strip_base(&installed, CONDA_BASE_PACKAGES);
        assert_eq!(result, vec!["scipy".to_string()]);
    }

    #[test]
    fn empty_installed_returns_empty() {
        let installed: Vec<String> = vec![];
        assert!(strip_base(&installed, UV_BASE_PACKAGES).is_empty());
    }

    #[test]
    fn installed_all_base_returns_empty() {
        let installed: Vec<String> = UV_BASE_PACKAGES.iter().map(|s| s.to_string()).collect();
        assert!(strip_base(&installed, UV_BASE_PACKAGES).is_empty());
    }

    #[test]
    fn empty_base_returns_all() {
        let installed: Vec<String> = vec!["pandas".into(), "numpy".into()];
        assert_eq!(strip_base(&installed, &[]), installed);
    }

    #[test]
    fn preserves_order() {
        let installed: Vec<String> = ["pandas", "ipykernel", "numpy", "uv", "matplotlib"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            strip_base(&installed, UV_BASE_PACKAGES),
            vec![
                "pandas".to_string(),
                "numpy".to_string(),
                "matplotlib".to_string()
            ]
        );
    }
}

#[cfg(all(test, feature = "runtime"))]
mod site_packages_has_ipykernel_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_site_packages_with(files: &[(&str, &str)]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        for (name, kind) in files {
            let path = tmp.path().join(name);
            match *kind {
                "dir" => std::fs::create_dir_all(&path).unwrap(),
                "file" => std::fs::write(&path, "").unwrap(),
                _ => panic!("unknown kind: {kind}"),
            }
        }
        tmp
    }

    #[test]
    fn no_ipykernel_returns_false() {
        let tmp = make_site_packages_with(&[("numpy", "dir"), ("pandas", "dir")]);
        assert!(!site_packages_has_ipykernel(tmp.path()));
    }

    #[test]
    fn ipykernel_package_dir_returns_true() {
        let tmp = make_site_packages_with(&[("ipykernel", "dir"), ("numpy", "dir")]);
        assert!(site_packages_has_ipykernel(tmp.path()));
    }

    #[test]
    fn ipykernel_dist_info_returns_true() {
        let tmp = make_site_packages_with(&[("ipykernel-6.29.5.dist-info", "dir")]);
        assert!(site_packages_has_ipykernel(tmp.path()));
    }

    #[test]
    fn empty_site_packages_returns_false() {
        let tmp = TempDir::new().unwrap();
        assert!(!site_packages_has_ipykernel(tmp.path()));
    }

    #[test]
    fn nonexistent_path_returns_false() {
        assert!(!site_packages_has_ipykernel(std::path::Path::new(
            "/definitely/does/not/exist"
        )));
    }

    #[test]
    fn similar_prefix_without_ipykernel_returns_false() {
        // Defensive: `ipykernel_something` must not trip the dist-info
        // fallback. Only `ipykernel-*.dist-info` counts.
        let tmp = make_site_packages_with(&[
            ("ipykernel_contrib", "dir"),
            ("ipykernelfoo-1.0.dist-info", "dir"),
        ]);
        assert!(!site_packages_has_ipykernel(tmp.path()));
    }

    #[test]
    fn venv_has_ipykernel_returns_false_for_nonexistent_interpreter() {
        // The outer `venv_has_ipykernel` delegates to a subprocess call
        // on the interpreter. Bad paths must surface as `false`, not panic.
        assert!(!venv_has_ipykernel(std::path::Path::new(
            "/definitely/not/a/python/interpreter"
        )));
    }

    #[test]
    fn sibling_site_packages_detects_abi_mismatch_candidate() {
        let tmp = TempDir::new().unwrap();
        let purelib = tmp.path().join("lib/python3.14t/site-packages");
        let sibling = tmp.path().join("lib/python3.14/site-packages");
        std::fs::create_dir_all(&purelib).unwrap();
        std::fs::create_dir_all(sibling.join("ipykernel")).unwrap();

        assert_eq!(
            sibling_site_packages_with_ipykernel(&purelib),
            vec![sibling]
        );
    }

    #[test]
    fn sibling_site_packages_ignores_current_purelib() {
        let tmp = TempDir::new().unwrap();
        let purelib = tmp.path().join("lib/python3.14/site-packages");
        std::fs::create_dir_all(purelib.join("ipykernel")).unwrap();

        assert!(sibling_site_packages_with_ipykernel(&purelib).is_empty());
    }
}
