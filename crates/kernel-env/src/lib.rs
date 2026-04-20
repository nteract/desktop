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
