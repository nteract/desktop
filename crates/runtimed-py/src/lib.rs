//! Python bindings for runtimed daemon client.
//!
//! Provides Python classes for:
//! - `NativeClient`/`NativeAsyncClient`: Daemon operations (status, ping, list active notebooks)
//! - `Session`: Synchronous notebook interaction with kernel management
//! - `AsyncSession`: Async notebook interaction with kernel management
//!
//! Both sync and async APIs are provided with full feature parity.

use pyo3::prelude::*;
use std::path::PathBuf;

mod async_client;
mod async_session;
mod client;
mod daemon_paths;
mod error;

mod output;
mod output_resolver;
mod session;
mod session_core;
mod subscription;

use async_client::AsyncClient;
use async_session::AsyncSession;
use client::Client;
use error::RuntimedError;

use output::{
    Cell, CompletionItem, CompletionResult, ExecutionEvent, ExecutionResult, HistoryEntry,
    NotebookConnectionInfo, Output, PyEnvState, PyKernelState, PyQueueEntry, PyRuntimeState,
    QueueState, SyncEnvironmentResult,
};
use session::Session;
use subscription::{EventIteratorSubscription, EventSubscription};

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
    ::runtimed::default_socket_path()
        .to_string_lossy()
        .to_string()
}

/// Python module for runtimed daemon client.
#[pymodule]
fn runtimed(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Core classes - new API (recommended)
    m.add_class::<Client>()?;
    m.add_class::<AsyncClient>()?;

    // Session types (used internally by Python wrappers)
    m.add_class::<Session>()?;
    m.add_class::<AsyncSession>()?;

    // Subscription types for independent event listening
    m.add_class::<EventSubscription>()?;
    m.add_class::<EventIteratorSubscription>()?;

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

    // Error type
    m.add("RuntimedError", m.py().get_type::<RuntimedError>())?;

    // Standalone functions
    m.add_function(wrap_pyfunction!(show_notebook_app, m)?)?;
    m.add_function(wrap_pyfunction!(default_socket_path, m)?)?;

    Ok(())
}
