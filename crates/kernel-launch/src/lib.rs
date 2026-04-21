//! Shared kernel launching and tool bootstrapping for Runt.
//!
//! This crate provides the core kernel launching functionality used by both
//! the Tauri notebook app and the runtimed daemon. It includes:
//!
//! - Tool bootstrapping (deno, uv, ruff, pixi) via GitHub downloads
//! - Environment creation (UV/Conda)
//! - Kernel process spawning
//!
//! # Tool Bootstrapping
//!
//! Tools are automatically downloaded from GitHub releases if not found on PATH:
//!
//! ```ignore
//! use kernel_launch::tools;
//!
//! let deno = tools::get_deno_path().await?;
//! let uv = tools::get_uv_path().await?;
//! let ruff = tools::get_ruff_path().await?;
//! ```

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod tools;

// Re-export commonly used items
pub use tools::{get_deno_path, get_ruff_path, get_uv_path, BootstrappedTool};
