//! Sketch: pixi_api integration for nteract kernel-env.
//!
//! This module is a design sketch for integrating pixi's Rust API into nteract's
//! environment management. It shows how we would use `pixi_api` to manage conda
//! and pypi dependencies in pixi workspaces, replacing our current approach of
//! shelling out to the `pixi` CLI.
//!
//! # Status: Research / Experimental
//!
//! This file documents the integration surface and design decisions. The
//! `Interface` implementation compiles; the workspace operations are sketched
//! as documentation since they depend on types (like `DependencyOptions`) that
//! don't derive Default and require careful construction.
//!
//! # Architecture
//!
//! The integration has three layers:
//!
//! 1. **Interface impl** -- Adapts pixi's `Interface` trait (user interaction)
//!    to nteract's headless daemon context (no TTY, no prompts).
//!
//! 2. **WorkspaceContext** -- Wraps pixi's workspace loading and provides
//!    dependency add/remove/list operations.
//!
//! 3. **Reporter bridge** -- (Future work) Bridges pixi's progress reporting
//!    system to nteract's `ProgressHandler` trait for real-time UI updates.
//!
//! # Key API surfaces from pixi_api
//!
//! ```text
//! pixi_api::Interface          -- Trait for user interaction (confirm, info, warning, etc.)
//! pixi_api::WorkspaceContext   -- Main API: add/remove deps, list packages, manage tasks
//! pixi_api::DefaultContext     -- Workspace-less context (search only)
//! pixi_core::Workspace         -- Loaded workspace (from pixi.toml or pyproject.toml)
//! pixi_core::workspace::WorkspaceLocator -- Discovers workspace from a path
//! ```
//!
//! # Why pixi_api instead of shelling out to `pixi`?
//!
//! 1. **No subprocess overhead** -- Direct Rust calls, no process spawning.
//! 2. **Structured data** -- No JSON parsing of CLI output.
//! 3. **Progress integration** -- Can bridge pixi's reporter system to our UI.
//! 4. **Lock file access** -- Can read pixi.lock programmatically for env info.
//! 5. **Workspace manipulation** -- Can modify pixi.toml without TOML string hacking.
//!
//! # Challenges discovered during research
//!
//! 1. **Dependency weight**: pixi_api pulls in ~347 additional crates (303 to 650).
//!    This includes the full UV resolver, AWS SDK (for S3 conda channels),
//!    jsonrpsee (for build backends), and many pixi-internal crates.
//!
//! 2. **Version pinning**: pixi's transitive deps (especially rattler_solve and
//!    the uv crates) must be pinned to exact versions matching pixi's Cargo.lock.
//!    rattler_solve 5.1.0 broke pixi's ExcludeNewer field; we must pin to 5.0.3.
//!
//! 3. **Workspace patches**: pixi requires `[patch.crates-io]` entries for
//!    reqwest-middleware (astral-sh fork) and version-ranges (pubgrub fork).
//!    These patches affect our entire workspace, not just the pixi_api feature.
//!
//! 4. **Rust toolchain**: pixi's transitive AWS deps require Rust 1.92+
//!    (we currently pin 1.90.0 in rust-toolchain.toml).
//!
//! 5. **API stability**: pixi_api is v0.1.0 and internal to pixi. The API
//!    surface may change without notice between pixi releases.
//!
//! # Workspace operations sketch
//!
//! ## Loading a workspace
//!
//! ```ignore
//! use pixi_api::core::Workspace;
//!
//! // Load from a pixi.toml path
//! let workspace = Workspace::from_path(&pixi_toml_path)?;
//! let ctx = pixi_api::WorkspaceContext::new(NteractInterface::new(None), workspace);
//! ```
//!
//! ## Adding conda dependencies
//!
//! ```ignore
//! use indexmap::IndexMap;
//! use pixi_api::rattler_conda_types::{MatchSpec, PackageName};
//! use pixi_api::manifest::SpecType;
//!
//! let mut specs = IndexMap::new();
//! let pkg_name: PackageName = "numpy".parse()?;
//! let match_spec = MatchSpec::from_str("numpy", Default::default())?;
//! specs.insert(pkg_name, match_spec);
//!
//! // DependencyOptions requires:
//! //   feature: FeatureName (use FeatureName::DEFAULT for the default feature)
//! //   platforms: Vec<Platform> (empty = current platform)
//! //   no_install: bool
//! //   lock_file_usage: LockFileUsage
//! let dep_options = pixi_api::workspace::DependencyOptions {
//!     feature: pixi_api::manifest::FeatureName::default(),
//!     platforms: vec![],
//!     no_install: false,
//!     lock_file_usage: pixi_api::core::environment::LockFileUsage::Update,
//! };
//!
//! // GitOptions requires: git (Option<Url>), reference (GitReference), subdir (Option<String>)
//! // For non-git deps, all fields are None/default
//! let git_options = pixi_api::workspace::GitOptions {
//!     git: None,
//!     reference: Default::default(), // GitReference::DefaultBranch
//!     subdir: None,
//! };
//!
//! let update = ctx.add_conda_deps(specs, SpecType::Run, dep_options, git_options).await?;
//! // `update` is Option<UpdateDeps> -- if Some, the lock file needs to be updated
//! ```
//!
//! ## Listing packages
//!
//! ```ignore
//! use pixi_api::core::environment::LockFileUsage;
//!
//! let packages = ctx.list_packages(
//!     None,    // no regex filter
//!     None,    // current platform
//!     None,    // default environment
//!     false,   // not explicit only
//!     false,   // don't skip install
//!     LockFileUsage::Update,
//! ).await?;
//!
//! for pkg in packages {
//!     println!("{} {} ({:?})", pkg.name, pkg.version, pkg.kind);
//! }
//! ```
//!
//! # Reporter bridge (future work)
//!
//! pixi uses `pixi_command_dispatcher::Reporter` for progress. Key sub-reporters:
//! - `CondaSolveReporter` -- conda dependency solving
//! - `PixiInstallReporter` -- package installation
//! - `GatewayReporter` (via `rattler_repodata_gateway::Reporter`) -- repodata fetching
//!
//! To bridge to our `ProgressHandler`, we would map pixi events to `EnvProgressPhase`:
//! - Solve start -> `EnvProgressPhase::Solving`
//! - Gateway events -> `EnvProgressPhase::FetchingRepodata`
//! - Install download -> `EnvProgressPhase::DownloadProgress`
//! - Install link -> `EnvProgressPhase::LinkProgress`
//!
//! Note: pixi_api itself does not use the Reporter trait directly -- it's used
//! by the `pixi_command_dispatcher` layer above. For pixi_api add/remove, the
//! solve+install happens internally without granular progress callbacks.
//!
//! # Alternative: lightweight pixi_manifest only
//!
//! If pixi_api is too heavy (347 extra crates), we could use just `pixi_manifest`:
//! - Parse pixi.toml to read deps, channels, platforms, environments
//! - Detect pixi workspaces during environment resolution
//! - But NOT add/remove deps, install packages, or read lock files
//!
//! This lighter approach may suffice for nteract's current needs (detecting pixi
//! workspaces and reading their configuration).
//!
//! # Integration with nteract's environment detection
//!
//! Currently nteract detects pixi workspaces by walking up from the notebook
//! directory, finding pixi.toml, and shelling out to `pixi info --json`.
//! With pixi_api, we could instead use `WorkspaceLocator` for discovery and
//! read the lock file directly for Python paths and installed packages.

#[cfg(feature = "pixi_api")]
use std::future::Future;
#[cfg(feature = "pixi_api")]
use std::sync::Arc;

/// Headless pixi interface for the nteract daemon.
///
/// The daemon has no TTY, so:
/// - `is_cli()` returns false (we're not a CLI)
/// - `confirm()` auto-accepts (daemon manages environments autonomously)
/// - Messages are routed to our logging system
#[cfg(feature = "pixi_api")]
pub struct NteractInterface {
    /// Optional progress handler for routing messages to the UI
    _handler: Option<Arc<dyn crate::progress::ProgressHandler>>,
}

#[cfg(feature = "pixi_api")]
impl NteractInterface {
    pub fn new(handler: Option<Arc<dyn crate::progress::ProgressHandler>>) -> Self {
        Self { _handler: handler }
    }
}

#[cfg(feature = "pixi_api")]
impl pixi_api::Interface for NteractInterface {
    fn is_cli(&self) -> impl Future<Output = bool> + Send {
        async { false }
    }

    fn confirm(&self, msg: &str) -> impl Future<Output = miette::Result<bool>> + Send {
        let msg = msg.to_string();
        async move {
            log::info!("[pixi] Auto-confirming: {}", msg);
            Ok(true)
        }
    }

    fn info(&self, msg: &str) -> impl Future<Output = ()> + Send {
        let msg = msg.to_string();
        async move {
            log::info!("[pixi] {}", msg);
        }
    }

    fn success(&self, msg: &str) -> impl Future<Output = ()> + Send {
        let msg = msg.to_string();
        async move {
            log::info!("[pixi] {}", msg);
        }
    }

    fn warning(&self, msg: &str) -> impl Future<Output = ()> + Send {
        let msg = msg.to_string();
        async move {
            log::warn!("[pixi] {}", msg);
        }
    }

    fn error(&self, msg: &str) -> impl Future<Output = ()> + Send {
        let msg = msg.to_string();
        async move {
            log::error!("[pixi] {}", msg);
        }
    }
}

/// Load a pixi workspace from a pixi.toml path.
///
/// This is the entry point for all pixi workspace operations. Returns a
/// `WorkspaceContext` that can be used for dependency management operations.
#[cfg(feature = "pixi_api")]
pub fn load_workspace(
    pixi_toml_path: &std::path::Path,
    handler: Option<Arc<dyn crate::progress::ProgressHandler>>,
) -> anyhow::Result<pixi_api::WorkspaceContext<NteractInterface>> {
    use pixi_api::core::Workspace;

    let interface = NteractInterface::new(handler);
    let workspace = Workspace::from_path(pixi_toml_path)
        .map_err(|e| anyhow::anyhow!("Failed to load pixi workspace: {}", e))?;

    Ok(pixi_api::WorkspaceContext::new(interface, workspace))
}
