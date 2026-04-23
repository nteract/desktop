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

/// Check if a prepared UV venv or Conda env has `ipykernel` installed in its
/// site-packages.
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
/// Returns `false` on any failure (interpreter missing, spawn error,
/// non-zero exit, empty output) — conservative: we'd rather surface a
/// misleading "missing ipykernel" than try to launch a broken env.
///
/// Scans for a direct child directory named `ipykernel` or any
/// `ipykernel-*.dist-info` metadata dir under site-packages.
#[cfg(feature = "runtime")]
pub fn venv_has_ipykernel(python_path: &std::path::Path) -> bool {
    let Some(site_packages) = resolve_site_packages(python_path) else {
        return false;
    };
    site_packages_has_ipykernel(&site_packages)
}

/// Ask the given interpreter for its site-packages path.
///
/// Uses `sysconfig.get_paths()['purelib']` which is the canonical
/// pure-Python install target for that interpreter. Returns `None` if
/// the interpreter cannot be executed or if the output is unexpected.
#[cfg(feature = "runtime")]
fn resolve_site_packages(python_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new(python_path)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_paths()['purelib'])",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sp = String::from_utf8(output.stdout).ok()?;
    let sp = sp.trim();
    if sp.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(sp);
    path.is_dir().then_some(path)
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
        let installed: Vec<String> = ["ipykernel", "ipywidgets", "anywidget", "nbformat", "scipy"]
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
    fn strips_dx_from_bootstrap_dx_envs() {
        // When bootstrap_dx is on, `uv_prewarmed_packages` adds `dx` to the
        // pool env's install list. `dx` is treated as base-level tooling
        // (the feature flag controls it, not the user), so strip_base
        // removes it — keeping captured notebook metadata free of the
        // feature flag's state.
        let installed: Vec<String> = [
            "ipykernel",
            "ipywidgets",
            "anywidget",
            "nbformat",
            "uv",
            "dx",
            "pandas",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            strip_base(&installed, UV_BASE_PACKAGES),
            vec!["pandas".to_string()]
        );
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
}
