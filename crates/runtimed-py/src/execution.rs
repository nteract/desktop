//! Execution handle — a Python-visible class that wraps an execution_id
//! and provides `result()`, `stream()`, `status`, and `cancel()`.
//!
//! The cell's outputs update through the CRDT regardless of whether
//! anyone holds this handle. It provides ways to observe the execution:
//!
//! - `await execution.result()` — block until done, return outputs
//! - `execution.stream()` — return an async iterator of events
//! - `execution.status` — sync read of current status
//! - `await execution.cancel()` — interrupt the execution

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use pyo3::prelude::*;

use crate::error::to_py_err;
use crate::event_stream::ExecutionEventStream;
use crate::output::{ExecutionResult, Output};
use crate::output_resolver;
use crate::session_core::{self, SessionState};

use notebook_protocol::protocol::NotebookBroadcast;

/// A handle to a submitted cell execution.
///
/// Returned by `session.execute(cell_id)`. The execution proceeds
/// regardless of whether this handle is used — the daemon drives it
/// and writes outputs into the CRDT. This handle just gives you ways
/// to observe or cancel the execution.
#[pyclass]
pub struct Execution {
    pub(crate) execution_id: String,
    pub(crate) cell_id: String,
    pub(crate) state: Arc<Mutex<SessionState>>,
    pub(crate) blob_base_url: Option<String>,
    pub(crate) blob_store_path: Option<PathBuf>,
}

#[pymethods]
impl Execution {
    #[getter]
    fn execution_id(&self) -> String {
        self.execution_id.clone()
    }

    #[getter]
    fn cell_id(&self) -> String {
        self.cell_id.clone()
    }

    /// Current execution status: "pending", "running", "done", or "error".
    ///
    /// Sync read from the local RuntimeStateDoc replica. Uses try_lock
    /// to avoid blocking — returns "running" if the lock is contended
    /// (another async operation is in progress, so the execution is
    /// likely still active).
    #[getter]
    fn status(&self) -> PyResult<String> {
        let st = match self.state.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                // Lock is held by an async operation (e.g. collect_outputs,
                // stream, or another concurrent call). The execution is
                // almost certainly still in flight.
                return Ok("running".to_string());
            }
        };

        if let Some(handle) = st.handle.as_ref() {
            if let Ok(rs) = handle.get_runtime_state() {
                if rs.kernel.status == "error" || rs.kernel.status == "shutdown" {
                    return Ok("error".to_string());
                }

                // Currently executing this execution?
                if let Some(ref entry) = rs.queue.executing {
                    if entry.execution_id == self.execution_id {
                        return Ok("running".to_string());
                    }
                }

                // Queued but not yet executing?
                if rs
                    .queue
                    .queued
                    .iter()
                    .any(|e| e.execution_id == self.execution_id)
                {
                    return Ok("pending".to_string());
                }

                // Not in queue — finished or never seen
                return Ok("done".to_string());
            }
        }

        Ok("done".to_string())
    }

    /// Wait for the execution to complete and return the result.
    ///
    /// Reads final outputs from the CRDT document after the cell finishes.
    ///
    /// Args:
    ///     timeout: Maximum seconds to wait (default: 60).
    #[pyo3(signature = (timeout=None))]
    fn result<'py>(&self, py: Python<'py>, timeout: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = self.cell_id.clone();
        let execution_id = self.execution_id.clone();
        let blob_base_url = self.blob_base_url.clone();
        let blob_store_path = self.blob_store_path.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let timeout_dur = std::time::Duration::from_secs_f64(timeout.unwrap_or(60.0));

            let result = tokio::time::timeout(timeout_dur, async {
                collect_outputs_for_execution(
                    &state,
                    &cell_id,
                    &execution_id,
                    blob_base_url,
                    blob_store_path,
                )
                .await
            })
            .await;

            match result {
                Ok(Ok(exec_result)) => Ok(exec_result),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(to_py_err(format!(
                    "Execution timed out after {} seconds",
                    timeout.unwrap_or(60.0)
                ))),
            }
        })
    }

    /// Return an async iterator of execution events.
    ///
    /// Each iteration yields an ExecutionEvent until the execution completes.
    /// Resubscribes from the session's broadcast receiver, so calling this
    /// multiple times is safe (though you'll miss events from before the call).
    ///
    /// Args:
    ///     timeout_secs: Maximum seconds to wait per event (default: 60).
    ///     signal_only: If True, output events contain only the index, not data.
    #[pyo3(signature = (timeout_secs=60.0, signal_only=false))]
    fn stream(&self, timeout_secs: f64, signal_only: bool) -> PyResult<ExecutionEventStream> {
        let st = self
            .state
            .try_lock()
            .map_err(|_| to_py_err("Session state locked"))?;

        let broadcast_rx = st
            .broadcast_rx
            .as_ref()
            .ok_or_else(|| to_py_err("No broadcast receiver"))?
            .resubscribe();

        Ok(ExecutionEventStream::new(
            broadcast_rx,
            self.cell_id.clone(),
            timeout_secs,
            st.blob_base_url.clone(),
            st.blob_store_path.clone(),
            signal_only,
        ))
    }

    /// Cancel this execution by interrupting the kernel.
    ///
    /// If the cell is currently running, sends an interrupt. If it's queued,
    /// this is currently a no-op (cancel-from-queue requires a new protocol
    /// variant).
    ///
    /// TODO: After interrupting, clear the execution queue so queued cells
    /// don't run. NotebookRequest doesn't have a ClearQueue variant yet —
    /// add one and send it here after the interrupt succeeds.
    fn cancel<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            session_core::interrupt(&state).await
            // TODO: send ClearQueue request here once the protocol supports it
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "Execution(cell_id='{}', execution_id='{}')",
            self.cell_id, self.execution_id
        )
    }
}

/// Wait for a specific execution to complete, then read outputs from the doc.
///
/// Adapted from `session_core::collect_outputs` but tracks by `execution_id`
/// rather than `cell_id`. This matters when a cell is re-executed: we want to
/// wait for *this* execution, not a previous one that already left the queue.
async fn collect_outputs_for_execution(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    execution_id: &str,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
) -> PyResult<ExecutionResult> {
    let mut kernel_error: Option<String> = None;

    // We must see the execution in the queue at least once before treating
    // its absence as completion. Otherwise we'd return immediately with
    // empty outputs because the RuntimeStateDoc hasn't synced yet.
    let mut seen_in_queue = false;

    loop {
        // Check RuntimeStateDoc queue state
        let cell_done_via_doc = {
            let st = state.lock().await;
            if let Some(handle) = st.handle.as_ref() {
                if let Ok(rs) = handle.get_runtime_state() {
                    // Check completion log first — handles late consumers who
                    // connect after the execution already finished. Without this,
                    // a late consumer would poll forever waiting for an execution
                    // that already left the queue.
                    if rs.completed.iter().any(|c| c.execution_id == execution_id) {
                        log::debug!(
                            "[execution] Execution {} found in completion log (late consumer fast path)",
                            execution_id
                        );
                        true
                    } else if rs.kernel.status == "error" {
                        kernel_error = Some("Kernel error".to_string());
                        true
                    } else if rs.kernel.status == "shutdown" {
                        kernel_error = Some("Kernel shut down".to_string());
                        true
                    } else {
                        // Check by execution_id, not cell_id
                        let in_executing =
                            rs.queue.executing.as_ref().map(|e| e.execution_id.as_str())
                                == Some(execution_id);
                        let in_queued = rs
                            .queue
                            .queued
                            .iter()
                            .any(|e| e.execution_id == execution_id);
                        let in_queue = in_executing || in_queued;

                        if in_queue {
                            seen_in_queue = true;
                            false // still running
                        } else if seen_in_queue {
                            // Was in queue, now gone → done
                            true
                        } else {
                            // Never seen — doc hasn't synced yet, keep polling
                            false
                        }
                    }
                } else {
                    false
                }
            } else {
                false
            }
        };

        if cell_done_via_doc {
            log::debug!(
                "[execution] Execution {} for cell {} left queue (RuntimeStateDoc)",
                execution_id,
                cell_id
            );
            break;
        }

        // Drain broadcasts as a fallback signal
        {
            let mut st = state.lock().await;
            if let Some(broadcast_rx) = st.broadcast_rx.as_mut() {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    broadcast_rx.recv(),
                )
                .await
                {
                    Ok(Some(NotebookBroadcast::ExecutionDone {
                        execution_id: msg_exec_id,
                        ..
                    })) => {
                        if msg_exec_id == execution_id {
                            log::debug!("[execution] ExecutionDone broadcast for {}", execution_id);
                            break;
                        }
                    }
                    Ok(Some(NotebookBroadcast::KernelError { error })) => {
                        log::debug!("[execution] KernelError: {}", error);
                        kernel_error = Some(error);
                        break;
                    }
                    Ok(Some(_)) => {} // ignore other broadcasts
                    Ok(None) => return Err(to_py_err("Broadcast channel closed")),
                    Err(_) => {} // timeout — loop back and re-check doc
                }
            }
        }
    }

    // Kernel error: return immediately without reading the doc
    if let Some(error) = kernel_error {
        return Ok(ExecutionResult {
            cell_id: cell_id.to_string(),
            outputs: vec![Output::error("KernelError", &error, vec![])],
            success: false,
            execution_count: None,
        });
    }

    // Phase 2: Read canonical cell state from the Automerge doc.
    // confirm_sync ensures our local replica has all outputs.
    let (snapshot, blob_base_url, blob_store_path) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle.confirm_sync().await.map_err(to_py_err)?;

        let snapshot = handle.get_cell(cell_id).ok_or_else(|| {
            to_py_err(format!(
                "Cell not found in doc after execution: {}",
                cell_id
            ))
        })?;

        (snapshot, blob_base_url, blob_store_path)
    };

    let execution_count = snapshot.execution_count.parse::<i64>().ok();
    let outputs =
        output_resolver::resolve_cell_outputs(&snapshot.outputs, &blob_base_url, &blob_store_path)
            .await;
    let success = !outputs.iter().any(|o| o.output_type == "error");

    Ok(ExecutionResult {
        cell_id: cell_id.to_string(),
        outputs,
        success,
        execution_count,
    })
}
