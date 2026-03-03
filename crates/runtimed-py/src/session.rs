//! Session for code execution.
//!
//! Provides a high-level interface for executing code via the daemon's kernel.

use pyo3::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use runtimed::notebook_sync_client::{
    NotebookBroadcastReceiver, NotebookSyncClient, NotebookSyncHandle, NotebookSyncReceiver,
};
use runtimed::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use crate::error::to_py_err;
use crate::output::{Cell, ExecutionResult, Output};

/// A session for executing code via the runtimed daemon.
///
/// Each session connects to a unique "virtual notebook" room in the daemon
/// and can launch a kernel and execute code. Sessions are isolated from
/// each other but multiple sessions can share the same kernel if they
/// use the same notebook_id.
///
/// Example:
///     session = Session()
///     session.start_kernel()
///     result = session.execute("print('hello')")
///     print(result.stdout)  # "hello\n"
#[pyclass]
pub struct Session {
    runtime: Runtime,
    state: Arc<Mutex<SessionState>>,
    notebook_id: String,
}

struct SessionState {
    handle: Option<NotebookSyncHandle>,
    /// Keep the sync receiver alive so the sync task doesn't exit
    #[allow(dead_code)]
    sync_rx: Option<NotebookSyncReceiver>,
    broadcast_rx: Option<NotebookBroadcastReceiver>,
    kernel_started: bool,
    env_source: Option<String>,
    /// Base URL for blob server (for resolving blob hashes)
    blob_base_url: Option<String>,
    /// Path to blob store directory (fallback for direct disk access)
    blob_store_path: Option<PathBuf>,
}

impl SessionState {
    fn new() -> Self {
        Self {
            handle: None,
            sync_rx: None,
            broadcast_rx: None,
            kernel_started: false,
            env_source: None,
            blob_base_url: None,
            blob_store_path: None,
        }
    }
}

#[pymethods]
impl Session {
    /// Create a new session.
    ///
    /// Args:
    ///     notebook_id: Optional unique identifier for this session.
    ///                  If not provided, a random UUID is generated.
    ///                  Multiple Session objects with the same notebook_id
    ///                  will share the same kernel.
    #[new]
    #[pyo3(signature = (notebook_id=None))]
    fn new(notebook_id: Option<String>) -> PyResult<Self> {
        let runtime = Runtime::new().map_err(to_py_err)?;
        let notebook_id =
            notebook_id.unwrap_or_else(|| format!("agent-session-{}", uuid::Uuid::new_v4()));

        Ok(Self {
            runtime,
            state: Arc::new(Mutex::new(SessionState::new())),
            notebook_id,
        })
    }

    /// Get the notebook ID for this session.
    #[getter]
    fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Check if the session is connected to the daemon.
    #[getter]
    fn is_connected(&self) -> bool {
        let state = self.runtime.block_on(self.state.lock());
        state.handle.is_some()
    }

    /// Check if a kernel has been started.
    #[getter]
    fn kernel_started(&self) -> bool {
        let state = self.runtime.block_on(self.state.lock());
        state.kernel_started
    }

    /// Get the environment source (e.g., "uv:prewarmed") if kernel is running.
    #[getter]
    fn env_source(&self) -> Option<String> {
        let state = self.runtime.block_on(self.state.lock());
        state.env_source.clone()
    }

    /// Connect to the daemon.
    ///
    /// This is called automatically by start_kernel() if not already connected.
    /// Respects the RUNTIMED_SOCKET_PATH environment variable if set.
    fn connect(&self) -> PyResult<()> {
        self.runtime.block_on(async {
            let mut state = self.state.lock().await;
            if state.handle.is_some() {
                return Ok(()); // Already connected
            }

            // Check for socket path override via environment variable
            let socket_path = if let Ok(path) = std::env::var("RUNTIMED_SOCKET_PATH") {
                std::path::PathBuf::from(path)
            } else {
                runtimed::default_socket_path()
            };

            let (handle, sync_rx, broadcast_rx, _cells, _notebook_path) =
                NotebookSyncClient::connect_split(socket_path.clone(), self.notebook_id.clone())
                    .await
                    .map_err(to_py_err)?;

            // Determine blob server URL and blob store path based on socket path
            // In dev mode, blob server runs on a per-worktree port
            let (blob_base_url, blob_store_path) = if let Some(parent) = socket_path.parent() {
                // Read daemon.json to get blob port
                let daemon_json = parent.join("daemon.json");
                let base_url = if daemon_json.exists() {
                    std::fs::read_to_string(&daemon_json)
                        .ok()
                        .and_then(|contents| {
                            serde_json::from_str::<serde_json::Value>(&contents).ok()
                        })
                        .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
                        .map(|port| format!("http://127.0.0.1:{}", port))
                } else {
                    None
                };

                // Blob store is at {daemon_dir}/blobs/
                let store_path = parent.join("blobs");
                let store_path = if store_path.exists() {
                    Some(store_path)
                } else {
                    None
                };

                (base_url, store_path)
            } else {
                (None, None)
            };

            state.handle = Some(handle);
            state.sync_rx = Some(sync_rx); // Keep alive so sync task doesn't exit
            state.broadcast_rx = Some(broadcast_rx);
            state.blob_base_url = blob_base_url;
            state.blob_store_path = blob_store_path;

            Ok(())
        })
    }

    /// Start a kernel for this session.
    ///
    /// Args:
    ///     kernel_type: Type of kernel ("python" or "deno"). Defaults to "python".
    ///     env_source: Environment source. Defaults to "auto" (auto-detect from
    ///         inline deps or project files). For Deno kernels, this is ignored
    ///         and always uses "deno".
    ///     notebook_path: Optional path to the notebook file on disk.
    ///         Used for project file detection (pyproject.toml, pixi.toml,
    ///         environment.yml) when env_source is "auto".
    ///
    /// If a kernel is already running for this session's notebook_id,
    /// this returns immediately without starting a new one.
    #[pyo3(signature = (kernel_type="python", env_source="auto", notebook_path=None))]
    fn start_kernel(
        &self,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&str>,
    ) -> PyResult<()> {
        // Ensure connected first
        self.connect()?;

        self.runtime.block_on(async {
            let mut state = self.state.lock().await;

            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::LaunchKernel {
                    kernel_type: kernel_type.to_string(),
                    env_source: env_source.to_string(),
                    notebook_path: notebook_path.map(|p| p.to_string()),
                })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::KernelLaunched {
                    env_source: actual_env,
                    ..
                } => {
                    state.kernel_started = true;
                    state.env_source = Some(actual_env);
                    Ok(())
                }
                NotebookResponse::KernelAlreadyRunning {
                    env_source: actual_env,
                    ..
                } => {
                    state.kernel_started = true;
                    state.env_source = Some(actual_env);
                    Ok(())
                }
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    // =========================================================================
    // Document Operations (write to automerge doc, synced to all clients)
    // =========================================================================

    /// Create a new cell in the automerge document.
    ///
    /// The cell is written to the shared document and synced to all connected
    /// clients. Use execute_cell() to execute it.
    ///
    /// Args:
    ///     source: The cell source code (default: empty string).
    ///     cell_type: Cell type - "code", "markdown", or "raw" (default: "code").
    ///     index: Position to insert the cell (default: append at end).
    ///
    /// Returns:
    ///     The cell ID (str).
    #[pyo3(signature = (source="", cell_type="code", index=None))]
    fn create_cell(&self, source: &str, cell_type: &str, index: Option<usize>) -> PyResult<String> {
        self.connect()?;

        let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            // Get current cell count to determine index
            let cells = handle.get_cells().await.map_err(to_py_err)?;
            let insert_index = index.unwrap_or(cells.len());

            // Add cell to document
            handle
                .add_cell(insert_index, &cell_id, cell_type)
                .await
                .map_err(to_py_err)?;

            // Set source if provided
            if !source.is_empty() {
                handle
                    .update_source(&cell_id, source)
                    .await
                    .map_err(to_py_err)?;
            }

            Ok(cell_id)
        })
    }

    /// Update a cell's source in the automerge document.
    ///
    /// The change is synced to all connected clients.
    ///
    /// Args:
    ///     cell_id: The cell ID.
    ///     source: The new source code.
    fn set_source(&self, cell_id: &str, source: &str) -> PyResult<()> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle
                .update_source(cell_id, source)
                .await
                .map_err(to_py_err)
        })
    }

    /// Get a cell from the automerge document.
    ///
    /// Args:
    ///     cell_id: The cell ID.
    ///
    /// Returns:
    ///     Cell object with id, cell_type, source, and execution_count.
    ///
    /// Raises:
    ///     RuntimedError: If cell not found.
    fn get_cell(&self, cell_id: &str) -> PyResult<Cell> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let cells = handle.get_cells().await.map_err(to_py_err)?;
            cells
                .into_iter()
                .find(|c| c.id == cell_id)
                .map(Cell::from_snapshot)
                .ok_or_else(|| to_py_err(format!("Cell not found: {}", cell_id)))
        })
    }

    /// Get all cells from the automerge document.
    ///
    /// Returns:
    ///     List of Cell objects.
    fn get_cells(&self) -> PyResult<Vec<Cell>> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let cells = handle.get_cells().await.map_err(to_py_err)?;
            Ok(cells.into_iter().map(Cell::from_snapshot).collect())
        })
    }

    /// Delete a cell from the automerge document.
    ///
    /// Args:
    ///     cell_id: The cell ID to delete.
    fn delete_cell(&self, cell_id: &str) -> PyResult<()> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle.delete_cell(cell_id).await.map_err(to_py_err)
        })
    }

    // =========================================================================
    // Metadata Operations (synced via automerge doc)
    // =========================================================================

    /// Set a metadata value in the automerge document.
    ///
    /// The value is synced to the daemon and all connected clients.
    /// Use the key "notebook_metadata" to set the NotebookMetadataSnapshot
    /// (JSON-encoded kernelspec, language_info, and runt config).
    ///
    /// Args:
    ///     key: The metadata key.
    ///     value: The metadata value (typically JSON).
    fn set_metadata(&self, key: &str, value: &str) -> PyResult<()> {
        self.connect()?;

        let key = key.to_string();
        let value = value.to_string();

        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle.set_metadata(&key, &value).await.map_err(to_py_err)
        })
    }

    /// Get a metadata value from the automerge document.
    ///
    /// Reads from the local replica of the automerge doc.
    ///
    /// Args:
    ///     key: The metadata key.
    ///
    /// Returns:
    ///     The metadata value (str) or None if not set.
    fn get_metadata(&self, key: &str) -> PyResult<Option<String>> {
        self.connect()?;

        let key = key.to_string();

        self.runtime.block_on(async {
            let state = self.state.lock().await;
            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle.get_metadata(&key).await.map_err(to_py_err)
        })
    }

    // =========================================================================
    // Execution (document-first: reads source from automerge doc)
    // =========================================================================

    /// Execute a cell by ID.
    ///
    /// The daemon reads the cell's source from the automerge document and
    /// executes it. This ensures all clients see the same code being executed.
    ///
    /// If a kernel isn't running yet, this will start one automatically.
    /// If a kernel is already running in the daemon (e.g., started by another
    /// client), it will reuse that kernel.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///     timeout_secs: Maximum time to wait for execution (default: 60).
    ///
    /// Returns:
    ///     ExecutionResult with outputs, success status, and execution count.
    ///
    /// Raises:
    ///     RuntimedError: If not connected, cell not found, or timeout.
    #[pyo3(signature = (cell_id, timeout_secs=60.0))]
    fn execute_cell(&self, cell_id: &str, timeout_secs: f64) -> PyResult<ExecutionResult> {
        let cell_id = cell_id.to_string();

        // Auto-start kernel if not running (will reuse existing kernel if one is running)
        {
            let state = self.runtime.block_on(self.state.lock());
            if !state.kernel_started {
                drop(state);
                self.start_kernel("python", "auto", None)?;
            }
        }

        self.runtime.block_on(async {
            let state = self.state.lock().await;

            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let blob_base_url = state.blob_base_url.clone();
            let blob_store_path = state.blob_store_path.clone();

            // Execute cell (daemon reads source from automerge doc)
            let response = handle
                .send_request(NotebookRequest::ExecuteCell {
                    cell_id: cell_id.clone(),
                })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::CellQueued { .. } => {}
                NotebookResponse::Error { error } => return Err(to_py_err(error)),
                other => return Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }

            drop(state); // Release lock before waiting for broadcasts

            // Wait for outputs
            let timeout = std::time::Duration::from_secs_f64(timeout_secs);
            let result = tokio::time::timeout(
                timeout,
                self.collect_outputs(&cell_id, blob_base_url, blob_store_path),
            )
            .await;

            match result {
                Ok(Ok(exec_result)) => Ok(exec_result),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(to_py_err(format!(
                    "Execution timed out after {} seconds",
                    timeout_secs
                ))),
            }
        })
    }

    /// Convenience method: create a cell, execute it, and return the result.
    ///
    /// This is a shortcut that combines create_cell() and execute_cell().
    /// The cell is written to the automerge document before execution,
    /// so other connected clients will see it.
    ///
    /// Args:
    ///     code: The code to execute.
    ///     timeout_secs: Maximum time to wait for execution (default: 60).
    ///
    /// Returns:
    ///     ExecutionResult with outputs, success status, and execution count.
    ///
    /// Raises:
    ///     RuntimedError: If not connected, kernel not started, or timeout.
    #[pyo3(signature = (code, timeout_secs=60.0))]
    fn run(&self, code: &str, timeout_secs: f64) -> PyResult<ExecutionResult> {
        // Create cell in document first
        let cell_id = self.create_cell(code, "code", None)?;

        // Then execute by ID (daemon reads from doc)
        self.execute_cell(&cell_id, timeout_secs)
    }

    /// Queue a cell for execution without waiting for the result.
    ///
    /// The daemon reads the cell's source from the automerge document and
    /// queues it for execution. Use get_cell() to poll for results.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///
    /// Raises:
    ///     RuntimedError: If not connected or cell not found.
    fn queue_cell(&self, cell_id: &str) -> PyResult<()> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;

            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            // Queue cell execution (daemon reads source from automerge doc)
            let response = handle
                .send_request(NotebookRequest::ExecuteCell {
                    cell_id: cell_id.to_string(),
                })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::CellQueued { .. } => Ok(()),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Interrupt the currently executing cell.
    fn interrupt(&self) -> PyResult<()> {
        self.runtime.block_on(async {
            let state = self.state.lock().await;

            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::InterruptExecution {})
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::InterruptSent {} => Ok(()),
                NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Shutdown the kernel.
    fn shutdown_kernel(&self) -> PyResult<()> {
        self.runtime.block_on(async {
            let mut state = self.state.lock().await;

            let handle = state
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::ShutdownKernel {})
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::KernelShuttingDown {} => {
                    state.kernel_started = false;
                    state.env_source = None;
                    Ok(())
                }
                NotebookResponse::NoKernel {} => {
                    state.kernel_started = false;
                    state.env_source = None;
                    Ok(())
                }
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    fn __repr__(&self) -> String {
        let state = self.runtime.block_on(self.state.lock());
        let status = if state.kernel_started {
            "kernel_running"
        } else if state.handle.is_some() {
            "connected"
        } else {
            "disconnected"
        };
        format!("Session(id={}, status={})", self.notebook_id, status)
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        // Shutdown kernel on exit if running
        let state = self.runtime.block_on(self.state.lock());
        if state.kernel_started {
            drop(state);
            let _ = self.shutdown_kernel();
        }
        Ok(false) // Don't suppress exceptions
    }

    /// Close the session and shutdown the kernel if running.
    ///
    /// This is equivalent to using the session as a context manager
    /// and exiting the context.
    fn close(&self) -> PyResult<()> {
        let state = self.runtime.block_on(self.state.lock());
        if state.kernel_started {
            drop(state);
            let _ = self.shutdown_kernel();
        }
        Ok(())
    }
}

impl Session {
    /// Collect outputs for a cell until ExecutionDone is received.
    ///
    /// Note: Due to the Jupyter shell/iopub race condition, error outputs
    /// may arrive AFTER ExecutionDone. We continue draining for a short
    /// time after ExecutionDone to catch straggling outputs.
    async fn collect_outputs(
        &self,
        cell_id: &str,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
    ) -> PyResult<ExecutionResult> {
        let mut outputs = Vec::new();
        let mut execution_count = None;
        let mut success = true;
        let mut done_received = false;

        loop {
            let mut state = self.state.lock().await;

            let broadcast_rx = state
                .broadcast_rx
                .as_mut()
                .ok_or_else(|| to_py_err("Not connected"))?;

            // Use a short timeout - shorter after done to drain quickly
            let timeout_ms = if done_received { 50 } else { 100 };
            let broadcast = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                broadcast_rx.recv(),
            )
            .await;

            match broadcast {
                Ok(Some(msg)) => {
                    drop(state); // Release lock while processing
                    log::debug!("[session] Received broadcast: {:?}", msg);

                    match msg {
                        NotebookBroadcast::ExecutionStarted {
                            cell_id: msg_cell_id,
                            execution_count: count,
                        } => {
                            if msg_cell_id == cell_id {
                                execution_count = Some(count);
                            }
                        }
                        NotebookBroadcast::Output {
                            cell_id: msg_cell_id,
                            output_type,
                            output_json,
                            output_index,
                        } => {
                            log::debug!(
                                "[session] Output broadcast: type={}, cell_id={}, output_index={:?}",
                                output_type,
                                msg_cell_id,
                                output_index
                            );
                            if msg_cell_id == cell_id {
                                if let Some(output) = self
                                    .parse_output(
                                        &output_type,
                                        &output_json,
                                        &blob_base_url,
                                        &blob_store_path,
                                    )
                                    .await
                                {
                                    log::debug!(
                                        "[session] Parsed output: type={}",
                                        output.output_type
                                    );
                                    if output.output_type == "error" {
                                        success = false;
                                    }
                                    // If output_index is provided, update in place; otherwise append
                                    if let Some(idx) = output_index {
                                        if idx < outputs.len() {
                                            outputs[idx] = output;
                                        } else {
                                            outputs.push(output);
                                        }
                                    } else {
                                        outputs.push(output);
                                    }
                                } else {
                                    log::debug!("[session] Failed to parse output");
                                }
                            }
                        }
                        NotebookBroadcast::ExecutionDone {
                            cell_id: msg_cell_id,
                        } => {
                            if msg_cell_id == cell_id {
                                // Don't break immediately - drain for a bit to catch
                                // straggling outputs due to shell/iopub race condition
                                log::debug!(
                                    "[session] ExecutionDone received, starting drain phase"
                                );
                                done_received = true;
                            }
                        }
                        NotebookBroadcast::KernelError { error } => {
                            success = false;
                            outputs.push(Output::error("KernelError", &error, vec![]));
                            done_received = true;
                        }
                        _ => {
                            // Ignore other broadcasts (KernelStatus, QueueChanged, etc.)
                        }
                    }
                }
                Ok(None) => {
                    // Channel closed
                    return Err(to_py_err("Broadcast channel closed"));
                }
                Err(_) => {
                    // Timeout - if we've seen ExecutionDone, we're done draining
                    if done_received {
                        log::debug!(
                            "[session] Drain timeout, finishing with {} outputs",
                            outputs.len()
                        );
                        break;
                    }
                    // Otherwise continue waiting
                }
            }
        }

        Ok(ExecutionResult {
            cell_id: cell_id.to_string(),
            outputs,
            success,
            execution_count,
        })
    }

    /// Parse an output from the daemon broadcast.
    ///
    /// The output_json field may be:
    /// 1. A blob hash (SHA-256) that needs to be fetched from the blob server
    /// 2. Inline JSON content
    async fn parse_output(
        &self,
        output_type: &str,
        output_json: &str,
        blob_base_url: &Option<String>,
        blob_store_path: &Option<PathBuf>,
    ) -> Option<Output> {
        // Try to parse output_json directly first
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_json) {
            return self.output_from_json(output_type, &parsed);
        }

        // If it looks like a blob hash (64 hex chars), try to resolve it
        if output_json.len() == 64 && output_json.chars().all(|c| c.is_ascii_hexdigit()) {
            log::debug!("[session] Detected blob hash: {}", output_json);
            log::debug!("[session] blob_store_path: {:?}", blob_store_path);
            log::debug!("[session] blob_base_url: {:?}", blob_base_url);
            // First try: read directly from disk (most reliable)
            if let Some(store_path) = blob_store_path {
                // Blob path is {store}/{prefix}/{rest} where prefix is first 2 chars
                let prefix = &output_json[..2];
                let rest = &output_json[2..];
                let blob_path = store_path.join(prefix).join(rest);
                log::debug!("[session] Trying blob path: {:?}", blob_path);

                match std::fs::read_to_string(&blob_path) {
                    Ok(contents) => {
                        log::debug!("[session] Read blob file, contents len: {}", contents.len());
                        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) {
                            return self
                                .output_from_manifest(output_type, &manifest, blob_store_path)
                                .await;
                        } else {
                            log::debug!("[session] Failed to parse manifest JSON");
                        }
                    }
                    Err(e) => {
                        log::debug!("[session] Failed to read blob file: {}", e);
                    }
                }
            }

            // Second try: fetch from blob server (may fail due to server issues)
            if let Some(base_url) = blob_base_url {
                let url = format!("{}/blobs/{}", base_url, output_json);
                if let Ok(response) = reqwest::get(&url).await {
                    if response.status().is_success() {
                        if let Ok(manifest) = response.json::<serde_json::Value>().await {
                            return self
                                .output_from_manifest(output_type, &manifest, blob_store_path)
                                .await;
                        }
                    }
                }
            }
        }

        // Fallback: create an error output to preserve failure semantics
        // If the original output_type was "error", this ensures success=false is set
        if output_type == "error" {
            Some(Output::error(
                "OutputParseError",
                &format!("Failed to parse error output: {}", output_json),
                vec![],
            ))
        } else {
            Some(Output::stream(
                "stderr",
                &format!("Failed to parse output: {}", output_json),
            ))
        }
    }

    /// Convert a parsed JSON value to an Output.
    fn output_from_json(&self, output_type: &str, json: &serde_json::Value) -> Option<Output> {
        match output_type {
            "stream" => {
                let name = json.get("name")?.as_str()?;
                let text = json.get("text")?.as_str()?;
                Some(Output::stream(name, text))
            }
            "display_data" => {
                let data = json.get("data")?.as_object()?;
                let mut output_data = HashMap::new();
                for (key, value) in data {
                    if let Some(s) = value.as_str() {
                        output_data.insert(key.clone(), s.to_string());
                    } else {
                        output_data.insert(key.clone(), value.to_string());
                    }
                }
                Some(Output::display_data(output_data))
            }
            "execute_result" => {
                let data = json.get("data")?.as_object()?;
                let execution_count = json.get("execution_count")?.as_i64()?;
                let mut output_data = HashMap::new();
                for (key, value) in data {
                    if let Some(s) = value.as_str() {
                        output_data.insert(key.clone(), s.to_string());
                    } else {
                        output_data.insert(key.clone(), value.to_string());
                    }
                }
                Some(Output::execute_result(output_data, execution_count))
            }
            "error" => {
                let ename = json.get("ename")?.as_str()?.to_string();
                let evalue = json.get("evalue")?.as_str()?.to_string();
                let traceback = json
                    .get("traceback")?
                    .as_array()?
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                Some(Output::error(&ename, &evalue, traceback))
            }
            _ => None,
        }
    }

    /// Convert a blob manifest to an Output.
    ///
    /// The manifest has a format like:
    /// {"output_type": "stream", "name": "stdout", "text": {"inline": "..."}}
    async fn output_from_manifest(
        &self,
        output_type: &str,
        manifest: &serde_json::Value,
        blob_store_path: &Option<PathBuf>,
    ) -> Option<Output> {
        match output_type {
            "stream" => {
                let name = manifest.get("name")?.as_str()?;
                let text_ref = manifest.get("text")?;
                let text = self.resolve_content_ref(text_ref, blob_store_path).await?;
                Some(Output::stream(name, &text))
            }
            "display_data" | "execute_result" => {
                let data_map = manifest.get("data")?.as_object()?;
                let mut output_data = HashMap::new();

                for (mime_type, content_ref) in data_map {
                    if let Some(content) =
                        self.resolve_content_ref(content_ref, blob_store_path).await
                    {
                        output_data.insert(mime_type.clone(), content);
                    }
                }

                if output_type == "execute_result" {
                    let execution_count = manifest.get("execution_count")?.as_i64()?;
                    Some(Output::execute_result(output_data, execution_count))
                } else {
                    Some(Output::display_data(output_data))
                }
            }
            "error" => {
                let ename = manifest.get("ename")?.as_str()?.to_string();
                let evalue = manifest.get("evalue")?.as_str()?.to_string();

                // Traceback can be a content ref ({"inline": "[...]"}) or a direct array
                let traceback_val = manifest.get("traceback")?;
                let traceback = if let Some(arr) = traceback_val.as_array() {
                    // Direct array
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                } else {
                    // Content reference - resolve it and parse as JSON array
                    let tb_str = self
                        .resolve_content_ref(traceback_val, blob_store_path)
                        .await?;
                    serde_json::from_str::<Vec<String>>(&tb_str).ok()?
                };

                Some(Output::error(&ename, &evalue, traceback))
            }
            _ => None,
        }
    }

    /// Resolve a content reference from a blob manifest.
    ///
    /// Content refs can be:
    /// - {"inline": "actual content"} - content is inline
    /// - {"blob": "hash"} - content is in blob store
    async fn resolve_content_ref(
        &self,
        content_ref: &serde_json::Value,
        blob_store_path: &Option<PathBuf>,
    ) -> Option<String> {
        if let Some(inline) = content_ref.get("inline") {
            return inline.as_str().map(|s| s.to_string());
        }

        if let Some(blob_hash) = content_ref.get("blob").and_then(|v| v.as_str()) {
            // First try: read directly from disk
            if let Some(store_path) = blob_store_path {
                if blob_hash.len() >= 2 {
                    let prefix = &blob_hash[..2];
                    let rest = &blob_hash[2..];
                    let blob_path = store_path.join(prefix).join(rest);

                    if let Ok(contents) = std::fs::read_to_string(&blob_path) {
                        return Some(contents);
                    }
                }
            }

            // Second try: fetch from server
            let state = self.state.lock().await;
            if let Some(base_url) = &state.blob_base_url {
                let url = format!("{}/blobs/{}", base_url, blob_hash);
                drop(state);

                if let Ok(response) = reqwest::get(&url).await {
                    if response.status().is_success() {
                        return response.text().await.ok();
                    }
                }
            }
        }

        None
    }
}
