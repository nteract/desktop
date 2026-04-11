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

#[cfg(feature = "runtime")]
pub mod conda;
#[cfg(feature = "runtime")]
pub mod gc;
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
pub use conda::{CondaDependencies, CondaEnvironment};
pub use progress::{EnvProgressPhase, LogHandler, ProgressHandler};
#[cfg(feature = "runtime")]
pub use uv::{UvDependencies, UvEnvironment};
