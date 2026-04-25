//! Store directory resolution.
//!
//! `data_local_dir` on purpose — the allowlist is user decision
//! history, not cached data. Losing it breaks the UX contract.
//!
//! macOS: `~/Library/Application Support/runt/store/`
//! Linux: `~/.local/share/runt/store/`
//! Windows: `%LOCALAPPDATA%\runt\store\`

use std::path::PathBuf;

/// Default store directory, shared between stable and nightly channels
/// so the user's trust decisions carry across both (same convention as
/// the HMAC trust key in `runt-trust`).
pub fn default_store_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("runt").join("store"))
}

/// Explicit store directory under a caller-provided root. Used by
/// tests and benchmarks to isolate stores per process under `tempdir`.
pub fn store_dir_for(root: impl Into<PathBuf>) -> PathBuf {
    root.into().join("runt").join("store")
}
