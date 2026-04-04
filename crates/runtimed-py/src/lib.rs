//! Python bindings for runtimed daemon client.
//!
//! Provides Python classes for:
//! - `NativeAsyncClient`: Async daemon operations (status, ping, list active notebooks)
//! - `AsyncSession`: Async notebook interaction with kernel management

extern crate runtimed_client as runtimed;
use pyo3::prelude::*;
use std::path::PathBuf;

mod async_client;
mod async_session;
mod daemon_paths;
mod error;

mod output;
mod output_resolver;
mod session_core;

use async_client::AsyncClient;
use async_session::AsyncSession;
use error::RuntimedError;

use output::{
    Cell, CompletionItem, CompletionResult, ExecutionEvent, ExecutionResult, HistoryEntry,
    NotebookConnectionInfo, Output, PyCommDocEntry, PyEnvState, PyKernelState, PyQueueEntry,
    PyRuntimeState, QueueState, SyncEnvironmentResult,
};

/// Launch the desktop notebook app, optionally opening a specific notebook.
///
/// In dev mode, uses the local bundled binary. In production, tries installed
/// app candidates via platform-specific launch.
///
/// Args:
///     notebook_path: Optional filesystem path to the notebook to open.
///         Accepts str or pathlib.Path (any os.PathLike).
#[pyfunction]
#[pyo3(signature = (notebook_path=None))]
fn show_notebook_app(notebook_path: Option<PathBuf>) -> PyResult<()> {
    runt_workspace::open_notebook_app(notebook_path.as_deref(), &[])
        .map_err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>)
}

/// Get the default daemon socket path.
///
/// Respects the RUNTIMED_SOCKET_PATH environment variable if set.
/// In dev mode (RUNTIMED_WORKSPACE_PATH set), returns the per-worktree socket path.
#[pyfunction]
fn default_socket_path() -> String {
    runt_workspace::default_socket_path()
        .to_string_lossy()
        .to_string()
}

/// Parse a channel name string into a BuildChannel enum.
fn parse_channel(channel: &str) -> PyResult<runt_workspace::BuildChannel> {
    match channel {
        "stable" => Ok(runt_workspace::BuildChannel::Stable),
        "nightly" => Ok(runt_workspace::BuildChannel::Nightly),
        _ => Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
            "channel must be \"stable\" or \"nightly\", got {:?}",
            channel
        ))),
    }
}

/// Get the daemon socket path for a specific channel ("stable" or "nightly").
///
/// Unlike `default_socket_path()`, this ignores `RUNTIMED_SOCKET_PATH` and
/// returns the platform-correct path for the requested channel. Useful for
/// cross-channel discovery (e.g., a stable build connecting to nightly).
///
/// Args:
///     channel: Either "stable" or "nightly".
///
/// Raises:
///     ValueError: If channel is not "stable" or "nightly".
#[pyfunction]
fn socket_path_for_channel(channel: &str) -> PyResult<String> {
    let ch = parse_channel(channel)?;
    Ok(runt_workspace::socket_path_for_channel(ch)
        .to_string_lossy()
        .to_string())
}

/// Launch the desktop notebook app for a specific channel ("stable" or "nightly").
///
/// Like `show_notebook_app()`, but tries app candidates for the given channel
/// instead of the compile-time default.
///
/// Args:
///     channel: Either "stable" or "nightly".
///     notebook_path: Optional filesystem path to the notebook to open.
///
/// Raises:
///     ValueError: If channel is not "stable" or "nightly".
///     RuntimeError: If the app could not be launched.
#[pyfunction]
#[pyo3(signature = (channel, notebook_path=None))]
fn show_notebook_app_for_channel(channel: &str, notebook_path: Option<PathBuf>) -> PyResult<()> {
    let ch = parse_channel(channel)?;
    runt_workspace::open_notebook_app_for_channel(ch, notebook_path.as_deref(), &[])
        .map_err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>)
}

/// Python module for runtimed daemon client.
#[pymodule]
fn _internals(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Core classes
    m.add_class::<AsyncClient>()?;

    // Session types (used internally by Python wrappers)
    m.add_class::<AsyncSession>()?;

    // Output types
    m.add_class::<Cell>()?;
    m.add_class::<ExecutionResult>()?;
    m.add_class::<ExecutionEvent>()?;
    m.add_class::<Output>()?;
    m.add_class::<SyncEnvironmentResult>()?;
    m.add_class::<NotebookConnectionInfo>()?;

    // Completion and queue types
    m.add_class::<CompletionItem>()?;
    m.add_class::<CompletionResult>()?;
    m.add_class::<PyQueueEntry>()?;
    m.add_class::<QueueState>()?;
    m.add_class::<HistoryEntry>()?;

    // Runtime state types (from RuntimeStateDoc)
    m.add_class::<PyRuntimeState>()?;
    m.add_class::<PyKernelState>()?;
    m.add_class::<PyEnvState>()?;
    m.add_class::<PyCommDocEntry>()?;

    // Error type
    m.add("RuntimedError", m.py().get_type::<RuntimedError>())?;

    // Standalone functions
    m.add_function(wrap_pyfunction!(show_notebook_app, m)?)?;
    m.add_function(wrap_pyfunction!(show_notebook_app_for_channel, m)?)?;
    m.add_function(wrap_pyfunction!(default_socket_path, m)?)?;
    m.add_function(wrap_pyfunction!(socket_path_for_channel, m)?)?;

    Ok(())
}
