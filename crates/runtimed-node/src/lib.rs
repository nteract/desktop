//! Node.js (napi-rs) bindings for the runtimed daemon client.
//!
//! This crate is the Node/Deno/Bun analog of `runtimed-py`. It exposes a
//! minimal, promise-based API for opening a notebook against a running
//! `runtimed` daemon, creating and executing cells, and reading outputs.
//!
//! Phase 1 surface (intentionally small):
//!   - `defaultSocketPath()` / `socketPathForChannel(channel)`
//!   - `createNotebook({ runtime, workingDir?, socketPath? }) -> Session`
//!   - `openNotebook(notebookId, { socketPath? }) -> Session`
//!   - `Session.notebookId`
//!   - `Session.runCell(source, { timeoutMs? }) -> CellResult`
//!   - `Session.close()`
//!
//! Phase 2 (follow-on) will refactor the body of this crate to reuse a
//! shared `runtimed-session` crate extracted from `runtimed-py`.

#![deny(clippy::all)]

use napi_derive::napi;

mod error;
mod session;

pub use error::NodeError;
pub use session::{
    create_notebook, open_notebook, CellResult, CreateNotebookOptions, JsOutput,
    OpenNotebookOptions, RunCellOptions, Session,
};

/// Return the default daemon socket path.
///
/// Respects `RUNTIMED_SOCKET_PATH` if set. In dev mode
/// (`RUNTIMED_WORKSPACE_PATH` set) returns the per-worktree socket.
#[napi]
pub fn default_socket_path() -> String {
    runt_workspace::default_socket_path()
        .to_string_lossy()
        .to_string()
}

/// Return the daemon socket path for a specific channel ("stable" or
/// "nightly"). Ignores `RUNTIMED_SOCKET_PATH`.
#[napi]
pub fn socket_path_for_channel(channel: String) -> napi::Result<String> {
    let ch = match channel.as_str() {
        "stable" => runt_workspace::BuildChannel::Stable,
        "nightly" => runt_workspace::BuildChannel::Nightly,
        other => {
            return Err(napi::Error::from_reason(format!(
                "channel must be \"stable\" or \"nightly\", got {other:?}"
            )));
        }
    };
    Ok(runt_workspace::socket_path_for_channel(ch)
        .to_string_lossy()
        .to_string())
}

// Session + openNotebook/createNotebook are registered from session.rs
// via #[napi] so they show up on the generated `index.d.ts`.
