//! Shared kernel launching and tool bootstrapping for Runt.
//!
//! This crate provides the core kernel launching functionality used by both
//! the Tauri notebook app and the runtimed daemon. It includes:
//!
//! - Tool bootstrapping (deno, uv, ruff) via rattler
//! - Environment creation (UV/Conda)
//! - Kernel process spawning
//!
//! # Tool Bootstrapping
//!
//! Tools are automatically installed from conda-forge if not found on PATH:
//!
//! ```ignore
//! use kernel_launch::tools;
//!
//! let deno = tools::get_deno_path().await?;
//! let uv = tools::get_uv_path().await?;
//! let ruff = tools::get_ruff_path().await?;
//! ```

// Tests are allowed to use unwrap()/expect()—they're how you assert
// preconditions and keep test failures informative. Workspace-wide
// `clippy::unwrap_used = "warn"` applies to non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod tools;

// Re-export commonly used items
pub use tools::{get_deno_path, get_ruff_path, get_uv_path, BootstrappedTool};
