//! Python bindings for runtimed daemon client.
//!
//! Provides Python classes for:
//! - `DaemonClient`: Low-level daemon operations (status, ping, list rooms)
//! - `Session`: Synchronous code execution with kernel management
//! - `AsyncSession`: Async code execution with kernel management
//! - `ExecutionEventStream`: Async iterator over execution events
//! - `ExecutionEventIterator`: Sync iterator over execution events
//!
//! Both sync and async APIs are provided with full feature parity.

use pyo3::prelude::*;
use std::path::PathBuf;

mod async_session;
mod client;
mod daemon_paths;
mod error;
mod event_stream;
mod output;
mod output_resolver;
mod session;
mod session_core;
mod subscription;

use async_session::AsyncSession;
use client::DaemonClient;
use error::RuntimedError;
use event_stream::{ExecutionEventIterator, ExecutionEventStream};
use output::{
    Cell, CompletionItem, CompletionResult, ExecutionEvent, ExecutionResult, HistoryEntry,
    NotebookConnectionInfo, Output, QueueState, SyncEnvironmentResult,
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

/// Python module for runtimed daemon client.
#[pymodule]
fn runtimed(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Core classes - sync API
    m.add_class::<DaemonClient>()?;
    m.add_class::<Session>()?;

    // Core classes - async API
    m.add_class::<AsyncSession>()?;

    // Iterator types for streaming execution
    m.add_class::<ExecutionEventStream>()?;
    m.add_class::<ExecutionEventIterator>()?;

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
    m.add_class::<QueueState>()?;
    m.add_class::<HistoryEntry>()?;

    // Error type
    m.add("RuntimedError", m.py().get_type::<RuntimedError>())?;

    // Standalone functions
    m.add_function(wrap_pyfunction!(show_notebook_app, m)?)?;

    Ok(())
}
