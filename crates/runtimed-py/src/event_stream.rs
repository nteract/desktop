//! Async and sync iterators for execution events.
//!
//! Provides streaming access to execution events as they arrive from the kernel,
//! enabling real-time processing of outputs during cell execution.

use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use notebook_protocol::protocol::NotebookBroadcast;
use notebook_sync::BroadcastReceiver;

use crate::error::to_py_err;
use crate::output::ExecutionEvent;
use crate::output_resolver;

/// Async iterator over execution events for a cell.
///
/// Yields `ExecutionEvent` objects as they arrive from the kernel:
/// - `execution_started`: Execution began (has `execution_count`)
/// - `output`: An output was produced (has `output`)
/// - `done`: Execution finished
/// - `error`: Kernel error occurred (has `error_message`)
///
/// Example:
///     ```python
///     async for event in await session.stream_execute(cell_id):
///         if event.event_type == "output":
///             print(event.output.text)
///     ```
#[pyclass]
pub struct ExecutionEventStream {
    state: Arc<Mutex<EventStreamState>>,
}

struct EventStreamState {
    /// Broadcast receiver for this stream (resubscribed from session)
    broadcast_rx: BroadcastReceiver,
    /// Cell ID we're streaming events for
    cell_id: String,
    /// Whether execution is done
    done: bool,
    /// Timeout for waiting on events
    timeout_secs: f64,
    /// For resolving blob outputs
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
    /// Whether to include output data or just signal (signal_only mode)
    signal_only: bool,
}

impl ExecutionEventStream {
    /// Create a new execution event stream.
    pub fn new(
        broadcast_rx: BroadcastReceiver,
        cell_id: String,
        timeout_secs: f64,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
        signal_only: bool,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(EventStreamState {
                broadcast_rx,
                cell_id,
                done: false,
                timeout_secs,
                blob_base_url,
                blob_store_path,
                signal_only,
            })),
        }
    }
}

#[pymethods]
impl ExecutionEventStream {
    /// Return self as the async iterator.
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Get the next event asynchronously.
    ///
    /// Returns a coroutine that yields the next ExecutionEvent,
    /// or raises StopAsyncIteration when execution is complete.
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let mut state = state.lock().await;

            // If already done, signal end of iteration
            if state.done {
                return Err(PyStopAsyncIteration::new_err("Execution complete"));
            }

            let timeout = Duration::from_secs_f64(state.timeout_secs);
            let cell_id = state.cell_id.clone();
            let blob_base_url = state.blob_base_url.clone();
            let blob_store_path = state.blob_store_path.clone();
            let signal_only = state.signal_only;

            // Wait for next relevant broadcast
            loop {
                let recv_result = tokio::time::timeout(timeout, state.broadcast_rx.recv()).await;

                match recv_result {
                    Ok(Some(broadcast)) => {
                        match broadcast {
                            NotebookBroadcast::ExecutionStarted {
                                cell_id: msg_cell_id,
                                execution_count,
                                ..
                            } => {
                                if msg_cell_id == cell_id {
                                    return Ok(ExecutionEvent::execution_started(
                                        &cell_id,
                                        execution_count,
                                    ));
                                }
                                // Not our cell, continue waiting
                            }
                            NotebookBroadcast::Output {
                                cell_id: msg_cell_id,
                                output_type,
                                output_json,
                                output_index,
                                ..
                            } => {
                                if msg_cell_id == cell_id {
                                    if signal_only {
                                        // Signal-only mode: include index but not resolved output
                                        return Ok(ExecutionEvent::output_signal(
                                            &cell_id,
                                            output_index,
                                        ));
                                    } else {
                                        // Full mode: resolve and include the output
                                        if let Some(output) =
                                            output_resolver::resolve_output_with_type(
                                                &output_type,
                                                &output_json,
                                                &blob_base_url,
                                                &blob_store_path,
                                            )
                                            .await
                                        {
                                            return Ok(ExecutionEvent::output_with_index(
                                                &cell_id,
                                                output,
                                                output_index,
                                            ));
                                        }
                                    }
                                }
                            }
                            NotebookBroadcast::ExecutionDone {
                                cell_id: msg_cell_id,
                                ..
                            } => {
                                if msg_cell_id == cell_id {
                                    state.done = true;
                                    return Ok(ExecutionEvent::done(&cell_id));
                                }
                            }
                            NotebookBroadcast::KernelError { error } => {
                                state.done = true;
                                return Ok(ExecutionEvent::error(&cell_id, &error));
                            }
                            _ => {
                                // Ignore other broadcasts (KernelStatus, QueueChanged, etc.)
                                continue;
                            }
                        }
                    }
                    Ok(None) => {
                        // Channel closed
                        state.done = true;
                        return Err(to_py_err("Broadcast channel closed"));
                    }
                    Err(_) => {
                        // Timeout - execution might be hung
                        state.done = true;
                        return Err(to_py_err(format!(
                            "Execution timed out after {} seconds",
                            state.timeout_secs
                        )));
                    }
                }
            }
        })
    }

    fn __repr__(&self) -> String {
        "ExecutionEventStream(...)".to_string()
    }
}

/// Sync iterator over execution events for a cell.
///
/// This is the synchronous equivalent of `ExecutionEventStream`.
/// It blocks on each iteration until the next event arrives.
#[pyclass]
pub struct ExecutionEventIterator {
    runtime: tokio::runtime::Runtime,
    state: Arc<Mutex<EventStreamState>>,
}

impl ExecutionEventIterator {
    /// Create a new execution event iterator.
    pub fn new(
        broadcast_rx: BroadcastReceiver,
        cell_id: String,
        timeout_secs: f64,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
        signal_only: bool,
    ) -> PyResult<Self> {
        let runtime = tokio::runtime::Runtime::new().map_err(to_py_err)?;
        Ok(Self {
            runtime,
            state: Arc::new(Mutex::new(EventStreamState {
                broadcast_rx,
                cell_id,
                done: false,
                timeout_secs,
                blob_base_url,
                blob_store_path,
                signal_only,
            })),
        })
    }
}

#[pymethods]
impl ExecutionEventIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self) -> PyResult<Option<ExecutionEvent>> {
        let state = Arc::clone(&self.state);

        self.runtime.block_on(async {
            let mut state = state.lock().await;

            if state.done {
                return Ok(None);
            }

            let timeout = Duration::from_secs_f64(state.timeout_secs);
            let cell_id = state.cell_id.clone();
            let blob_base_url = state.blob_base_url.clone();
            let blob_store_path = state.blob_store_path.clone();
            let signal_only = state.signal_only;

            loop {
                let recv_result = tokio::time::timeout(timeout, state.broadcast_rx.recv()).await;

                match recv_result {
                    Ok(Some(broadcast)) => match broadcast {
                        NotebookBroadcast::ExecutionStarted {
                            cell_id: msg_cell_id,
                            execution_count,
                            ..
                        } => {
                            if msg_cell_id == cell_id {
                                return Ok(Some(ExecutionEvent::execution_started(
                                    &cell_id,
                                    execution_count,
                                )));
                            }
                        }
                        NotebookBroadcast::Output {
                            cell_id: msg_cell_id,
                            output_type,
                            output_json,
                            output_index,
                            ..
                        } => {
                            if msg_cell_id == cell_id {
                                if signal_only {
                                    return Ok(Some(ExecutionEvent::output_signal(
                                        &cell_id,
                                        output_index,
                                    )));
                                } else if let Some(output) =
                                    output_resolver::resolve_output_with_type(
                                        &output_type,
                                        &output_json,
                                        &blob_base_url,
                                        &blob_store_path,
                                    )
                                    .await
                                {
                                    return Ok(Some(ExecutionEvent::output_with_index(
                                        &cell_id,
                                        output,
                                        output_index,
                                    )));
                                }
                            }
                        }
                        NotebookBroadcast::ExecutionDone {
                            cell_id: msg_cell_id,
                            ..
                        } => {
                            if msg_cell_id == cell_id {
                                state.done = true;
                                return Ok(Some(ExecutionEvent::done(&cell_id)));
                            }
                        }
                        NotebookBroadcast::KernelError { error } => {
                            state.done = true;
                            return Ok(Some(ExecutionEvent::error(&cell_id, &error)));
                        }
                        _ => continue,
                    },
                    Ok(None) => {
                        state.done = true;
                        return Err(to_py_err("Broadcast channel closed"));
                    }
                    Err(_) => {
                        state.done = true;
                        return Err(to_py_err(format!(
                            "Execution timed out after {} seconds",
                            state.timeout_secs
                        )));
                    }
                }
            }
        })
    }

    fn __repr__(&self) -> String {
        "ExecutionEventIterator(...)".to_string()
    }
}
