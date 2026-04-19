//! Pure-Rust compute kernels for dataframe/Arrow analysis.
//!
//! Used by:
//! - `sift-wasm` (compiled to WASM for the @nteract/sift frontend)
//! - `runt-mcp` / `repr-llm` (native, for server-side dataframe summarization)
//!
//! This crate is intentionally free of `wasm-bindgen` so it compiles as
//! a plain `rlib` in native builds without pulling in JS interop code.

// Tests are allowed to use unwrap()/expect()—they're how you assert
// preconditions and keep test failures informative. Workspace-wide
// `clippy::unwrap_used = "warn"` applies to non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod arrow_utils;
pub mod filter;
pub mod parquet_summary;
pub mod summary;
pub mod utils;

pub use filter::{filter_rows, string_contains};
pub use parquet_summary::{summarize_parquet, ColumnStats, ColumnSummary, ParquetSummary};
pub use summary::{histogram, value_counts, CategoryCount, HistogramBin};
