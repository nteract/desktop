//! Async session for code execution.
//!
//! Provides an async interface for executing code via the daemon's kernel.
//! All methods return Python coroutines that can be awaited.

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use runtimed::notebook_sync_client::{
    NotebookBroadcastReceiver, NotebookSyncClient, NotebookSyncHandle, NotebookSyncReceiver,
};
use runtimed::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use crate::error::to_py_err;
use crate::output::{Cell, ExecutionResult, Output};

/// An async session for executing code via the runtimed daemon.
///
/// Each session connects to a unique "virtual notebook" room in the daemon
/// and can launch a kernel and execute code. Sessions are isolated from
/// each other but multiple sessions can share the same kernel if they
/// use the same notebook_id.
///
/// Example:
///     async with AsyncSession() as session:
///         await session.start_kernel()
///         cell_id = await session.create_cell("print('hello')")
///         result = await session.execute_cell(cell_id)
///         print(result.stdout)  # "hello\n"
#[pyclass]
pub struct AsyncSession {
    state: Arc<Mutex<AsyncSessionState>>,
    notebook_id: String,
}

struct AsyncSessionState {
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

impl AsyncSessionState {
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
impl AsyncSession {
    /// Create a new async session.
    ///
    /// Args:
    ///     notebook_id: Optional unique identifier for this session.
    ///                  If not provided, a random UUID is generated.
    ///                  Multiple AsyncSession objects with the same notebook_id
    ///                  will share the same kernel.
    #[new]
    #[pyo3(signature = (notebook_id=None))]
    fn new(notebook_id: Option<String>) -> PyResult<Self> {
        let notebook_id =
            notebook_id.unwrap_or_else(|| format!("agent-session-{}", uuid::Uuid::new_v4()));

        Ok(Self {
            state: Arc::new(Mutex::new(AsyncSessionState::new())),
            notebook_id,
        })
    }

    /// Get the notebook ID for this session.
    #[getter]
    fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Check if the session is connected to the daemon.
    ///
    /// Returns a coroutine that resolves to bool.
    fn is_connected<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let state = state.lock().await;
            Ok(state.handle.is_some())
        })
    }

    /// Check if a kernel has been started.
    ///
    /// Returns a coroutine that resolves to bool.
    fn kernel_started<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let state = state.lock().await;
            Ok(state.kernel_started)
        })
    }

    /// Get the environment source (e.g., "uv:prewarmed") if kernel is running.
    ///
    /// Returns a coroutine that resolves to Optional[str].
    fn env_source<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let state = state.lock().await;
            Ok(state.env_source.clone())
        })
    }

    /// Connect to the daemon.
    ///
    /// This is called automatically by start_kernel() if not already connected.
    /// Respects the RUNTIMED_SOCKET_PATH environment variable if set.
    ///
    /// Returns a coroutine.
    fn connect<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();

        future_into_py(py, async move {
            let mut state = state.lock().await;
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
                NotebookSyncClient::connect_split(socket_path.clone(), notebook_id)
                    .await
                    .map_err(to_py_err)?;

            // Determine blob server URL and blob store path based on socket path
            let (blob_base_url, blob_store_path) = if let Some(parent) = socket_path.parent() {
                let daemon_json = parent.join("daemon.json");
                let base_url = if daemon_json.exists() {
                    tokio::fs::read_to_string(&daemon_json)
                        .await
                        .ok()
                        .and_then(|contents| {
                            serde_json::from_str::<serde_json::Value>(&contents).ok()
                        })
                        .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
                        .map(|port| format!("http://127.0.0.1:{}", port))
                } else {
                    None
                };

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
            state.sync_rx = Some(sync_rx);
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
    ///         notebook metadata or project files). For Deno kernels, this is
    ///         ignored and always uses "deno".
    ///
    /// If a kernel is already running for this session's notebook_id,
    /// this returns immediately without starting a new one.
    ///
    /// Returns a coroutine.
    #[pyo3(signature = (kernel_type="python", env_source="auto"))]
    fn start_kernel<'py>(
        &self,
        py: Python<'py>,
        kernel_type: &str,
        env_source: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let kernel_type = kernel_type.to_string();
        let env_source = env_source.to_string();

        future_into_py(py, async move {
            // Ensure connected first
            {
                let state_guard = state.lock().await;
                if state_guard.handle.is_none() {
                    drop(state_guard);

                    // Connect
                    let socket_path = if let Ok(path) = std::env::var("RUNTIMED_SOCKET_PATH") {
                        std::path::PathBuf::from(path)
                    } else {
                        runtimed::default_socket_path()
                    };

                    let (handle, sync_rx, broadcast_rx, _cells, _notebook_path) =
                        NotebookSyncClient::connect_split(socket_path.clone(), notebook_id)
                            .await
                            .map_err(to_py_err)?;

                    let (blob_base_url, blob_store_path) =
                        if let Some(parent) = socket_path.parent() {
                            let daemon_json = parent.join("daemon.json");
                            let base_url = if daemon_json.exists() {
                                tokio::fs::read_to_string(&daemon_json)
                                    .await
                                    .ok()
                                    .and_then(|contents| {
                                        serde_json::from_str::<serde_json::Value>(&contents).ok()
                                    })
                                    .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
                                    .map(|port| format!("http://127.0.0.1:{}", port))
                            } else {
                                None
                            };

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

                    let mut state_guard2 = state.lock().await;
                    state_guard2.handle = Some(handle);
                    state_guard2.sync_rx = Some(sync_rx);
                    state_guard2.broadcast_rx = Some(broadcast_rx);
                    state_guard2.blob_base_url = blob_base_url;
                    state_guard2.blob_store_path = blob_store_path;
                }
            }

            let mut state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::LaunchKernel {
                    kernel_type,
                    env_source,
                    notebook_path: None,
                })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::KernelLaunched {
                    env_source: actual_env,
                    ..
                } => {
                    state_guard.kernel_started = true;
                    state_guard.env_source = Some(actual_env);
                    Ok(())
                }
                NotebookResponse::KernelAlreadyRunning {
                    env_source: actual_env,
                    ..
                } => {
                    state_guard.kernel_started = true;
                    state_guard.env_source = Some(actual_env);
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
    /// Returns a coroutine that resolves to the cell ID (str).
    #[pyo3(signature = (source="", cell_type="code", index=None))]
    fn create_cell<'py>(
        &self,
        py: Python<'py>,
        source: &str,
        cell_type: &str,
        index: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let source = source.to_string();
        let cell_type = cell_type.to_string();

        future_into_py(py, async move {
            // Ensure connected
            {
                let state_guard = state.lock().await;
                if state_guard.handle.is_none() {
                    drop(state_guard);
                    return Err(to_py_err("Not connected. Call connect() first."));
                }
            }

            let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let cells = handle.get_cells().await.map_err(to_py_err)?;
            let insert_index = index.unwrap_or(cells.len());

            handle
                .add_cell(insert_index, &cell_id, &cell_type)
                .await
                .map_err(to_py_err)?;

            if !source.is_empty() {
                handle
                    .update_source(&cell_id, &source)
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
    ///
    /// Returns a coroutine.
    fn set_source<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        source: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let source = source.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle
                .update_source(&cell_id, &source)
                .await
                .map_err(to_py_err)
        })
    }

    /// Get a cell from the automerge document.
    ///
    /// Args:
    ///     cell_id: The cell ID.
    ///
    /// Returns a coroutine that resolves to Cell object.
    ///
    /// Raises:
    ///     RuntimedError: If cell not found.
    fn get_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
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
    /// Returns a coroutine that resolves to List[Cell].
    fn get_cells<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let cells = handle.get_cells().await.map_err(to_py_err)?;
            Ok(cells
                .into_iter()
                .map(Cell::from_snapshot)
                .collect::<Vec<_>>())
        })
    }

    /// Delete a cell from the automerge document.
    ///
    /// Args:
    ///     cell_id: The cell ID to delete.
    ///
    /// Returns a coroutine.
    fn delete_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle.delete_cell(&cell_id).await.map_err(to_py_err)
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
    /// Returns a coroutine that resolves to ExecutionResult.
    ///
    /// Raises:
    ///     RuntimedError: If not connected, cell not found, or timeout.
    #[pyo3(signature = (cell_id, timeout_secs=60.0))]
    fn execute_cell<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            // Auto-start kernel if not running
            {
                let state_guard = state.lock().await;
                if !state_guard.kernel_started {
                    drop(state_guard);

                    // Need to connect and start kernel
                    let state_guard = state.lock().await;
                    if state_guard.handle.is_none() {
                        drop(state_guard);

                        let socket_path = if let Ok(path) = std::env::var("RUNTIMED_SOCKET_PATH") {
                            std::path::PathBuf::from(path)
                        } else {
                            runtimed::default_socket_path()
                        };

                        let (handle, sync_rx, broadcast_rx, _cells, _notebook_path) =
                            NotebookSyncClient::connect_split(
                                socket_path.clone(),
                                notebook_id.clone(),
                            )
                            .await
                            .map_err(to_py_err)?;

                        let (blob_base_url, blob_store_path) = if let Some(parent) =
                            socket_path.parent()
                        {
                            let daemon_json = parent.join("daemon.json");
                            let base_url = if daemon_json.exists() {
                                tokio::fs::read_to_string(&daemon_json)
                                    .await
                                    .ok()
                                    .and_then(|contents| {
                                        serde_json::from_str::<serde_json::Value>(&contents).ok()
                                    })
                                    .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
                                    .map(|port| format!("http://127.0.0.1:{}", port))
                            } else {
                                None
                            };

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

                        let mut state_guard = state.lock().await;
                        state_guard.handle = Some(handle);
                        state_guard.sync_rx = Some(sync_rx);
                        state_guard.broadcast_rx = Some(broadcast_rx);
                        state_guard.blob_base_url = blob_base_url;
                        state_guard.blob_store_path = blob_store_path;
                    }

                    // Start kernel
                    let mut state_guard = state.lock().await;
                    let handle = state_guard
                        .handle
                        .as_ref()
                        .ok_or_else(|| to_py_err("Not connected"))?;

                    let response = handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: "python".to_string(),
                            env_source: "uv:prewarmed".to_string(),
                            notebook_path: None,
                        })
                        .await
                        .map_err(to_py_err)?;

                    match response {
                        NotebookResponse::KernelLaunched {
                            env_source: actual_env,
                            ..
                        } => {
                            state_guard.kernel_started = true;
                            state_guard.env_source = Some(actual_env);
                        }
                        NotebookResponse::KernelAlreadyRunning {
                            env_source: actual_env,
                            ..
                        } => {
                            state_guard.kernel_started = true;
                            state_guard.env_source = Some(actual_env);
                        }
                        NotebookResponse::Error { error } => return Err(to_py_err(error)),
                        other => {
                            return Err(to_py_err(format!("Unexpected response: {:?}", other)))
                        }
                    }
                }
            }

            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let blob_base_url = state_guard.blob_base_url.clone();
            let blob_store_path = state_guard.blob_store_path.clone();

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

            drop(state_guard); // Release lock before waiting for broadcasts

            // Wait for outputs
            let timeout = std::time::Duration::from_secs_f64(timeout_secs);
            let result = tokio::time::timeout(
                timeout,
                collect_outputs_async(&state, &cell_id, blob_base_url, blob_store_path),
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
    /// Returns a coroutine that resolves to ExecutionResult.
    ///
    /// Raises:
    ///     RuntimedError: If not connected, kernel not started, or timeout.
    #[pyo3(signature = (code, timeout_secs=60.0))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        code: &str,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let code = code.to_string();

        future_into_py(py, async move {
            // Create cell in document first
            let cell_id = {
                let state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

                let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

                // Get current cell count for append position
                let cells = handle.get_cells().await.map_err(to_py_err)?;
                let insert_index = cells.len();

                // Add cell to document
                handle
                    .add_cell(insert_index, &cell_id, "code")
                    .await
                    .map_err(to_py_err)?;

                // Set source
                handle
                    .update_source(&cell_id, &code)
                    .await
                    .map_err(to_py_err)?;

                cell_id
            };

            // Queue execution
            {
                let state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

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
            }

            // Get blob resolution config
            let (blob_base_url, blob_store_path) = {
                let state_guard = state.lock().await;
                (
                    state_guard.blob_base_url.clone(),
                    state_guard.blob_store_path.clone(),
                )
            };

            // Collect outputs with timeout
            let timeout = std::time::Duration::from_secs_f64(timeout_secs);
            let result = tokio::time::timeout(
                timeout,
                collect_outputs_async(&state, &cell_id, blob_base_url, blob_store_path),
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

    /// Queue a cell for execution without waiting for the result.
    ///
    /// The daemon reads the cell's source from the automerge document and
    /// queues it for execution. Use get_cell() to poll for results.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///
    /// Returns a coroutine.
    ///
    /// Raises:
    ///     RuntimedError: If not connected or cell not found.
    fn queue_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            // Queue cell execution (daemon reads source from automerge doc)
            let response = handle
                .send_request(NotebookRequest::ExecuteCell { cell_id })
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
    ///
    /// Returns a coroutine.
    fn interrupt<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
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
    ///
    /// Returns a coroutine.
    fn shutdown_kernel<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let mut state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::ShutdownKernel {})
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::KernelShuttingDown {} => {
                    state_guard.kernel_started = false;
                    state_guard.env_source = None;
                    Ok(())
                }
                NotebookResponse::NoKernel {} => {
                    state_guard.kernel_started = false;
                    state_guard.env_source = None;
                    Ok(())
                }
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Close the session and shutdown the kernel if running.
    ///
    /// Returns a coroutine.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let mut state_guard = state.lock().await;
            if state_guard.kernel_started {
                if let Some(handle) = state_guard.handle.as_ref() {
                    let _ = handle
                        .send_request(NotebookRequest::ShutdownKernel {})
                        .await;
                }
                state_guard.kernel_started = false;
                state_guard.env_source = None;
            }
            Ok(())
        })
    }

    fn __repr__(&self) -> String {
        format!("AsyncSession(id={})", self.notebook_id)
    }

    /// Async context manager entry.
    ///
    /// Returns a coroutine that resolves to self.
    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        // Return a coroutine that immediately resolves to self
        future_into_py(py, async move { Ok(slf) })
    }

    /// Async context manager exit.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let mut state_guard = state.lock().await;
            if state_guard.kernel_started {
                if let Some(handle) = state_guard.handle.as_ref() {
                    let _ = handle
                        .send_request(NotebookRequest::ShutdownKernel {})
                        .await;
                }
                state_guard.kernel_started = false;
                state_guard.env_source = None;
            }
            Ok(false) // Don't suppress exceptions
        })
    }
}

// =========================================================================
// Helper functions (outside impl block for async use)
// =========================================================================

/// Collect outputs for a cell until ExecutionDone is received.
async fn collect_outputs_async(
    state: &Arc<Mutex<AsyncSessionState>>,
    cell_id: &str,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
) -> PyResult<ExecutionResult> {
    let mut outputs = Vec::new();
    let mut execution_count = None;
    let mut success = true;
    let mut done_received = false;

    loop {
        let mut state_guard = state.lock().await;

        let broadcast_rx = state_guard
            .broadcast_rx
            .as_mut()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let timeout_ms = if done_received { 50 } else { 100 };
        let broadcast = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            broadcast_rx.recv(),
        )
        .await;

        match broadcast {
            Ok(Some(msg)) => {
                drop(state_guard);
                log::debug!("[async_session] Received broadcast: {:?}", msg);

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
                        if msg_cell_id == cell_id {
                            if let Some(output) = parse_output_async(
                                &output_type,
                                &output_json,
                                &blob_base_url,
                                &blob_store_path,
                            )
                            .await
                            {
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
                            }
                        }
                    }
                    NotebookBroadcast::ExecutionDone {
                        cell_id: msg_cell_id,
                    } => {
                        if msg_cell_id == cell_id {
                            log::debug!(
                                "[async_session] ExecutionDone received, starting drain phase"
                            );
                            done_received = true;
                        }
                    }
                    NotebookBroadcast::KernelError { error } => {
                        success = false;
                        outputs.push(Output::error("KernelError", &error, vec![]));
                        done_received = true;
                    }
                    _ => {}
                }
            }
            Ok(None) => {
                return Err(to_py_err("Broadcast channel closed"));
            }
            Err(_) => {
                if done_received {
                    log::debug!(
                        "[async_session] Drain timeout, finishing with {} outputs",
                        outputs.len()
                    );
                    break;
                }
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
async fn parse_output_async(
    output_type: &str,
    output_json: &str,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    // Try to parse output_json directly first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_json) {
        return output_from_json(output_type, &parsed);
    }

    // If it looks like a blob hash (64 hex chars), try to resolve it
    if output_json.len() == 64 && output_json.chars().all(|c| c.is_ascii_hexdigit()) {
        log::debug!("[async_session] Detected blob hash: {}", output_json);

        // First try: read directly from disk
        if let Some(store_path) = blob_store_path {
            let prefix = &output_json[..2];
            let rest = &output_json[2..];
            let blob_path = store_path.join(prefix).join(rest);

            if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) {
                    return output_from_manifest_async(output_type, &manifest, blob_store_path)
                        .await;
                }
            }
        }

        // Second try: fetch from blob server
        if let Some(base_url) = blob_base_url {
            let url = format!("{}/blobs/{}", base_url, output_json);
            if let Ok(response) = reqwest::get(&url).await {
                if response.status().is_success() {
                    if let Ok(manifest) = response.json::<serde_json::Value>().await {
                        return output_from_manifest_async(output_type, &manifest, blob_store_path)
                            .await;
                    }
                }
            }
        }
    }

    // Fallback
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
fn output_from_json(output_type: &str, json: &serde_json::Value) -> Option<Output> {
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
async fn output_from_manifest_async(
    output_type: &str,
    manifest: &serde_json::Value,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    match output_type {
        "stream" => {
            let name = manifest.get("name")?.as_str()?;
            let text_ref = manifest.get("text")?;
            let text = resolve_content_ref_async(text_ref, blob_store_path).await?;
            Some(Output::stream(name, &text))
        }
        "display_data" | "execute_result" => {
            let data_map = manifest.get("data")?.as_object()?;
            let mut output_data = HashMap::new();

            for (mime_type, content_ref) in data_map {
                if let Some(content) = resolve_content_ref_async(content_ref, blob_store_path).await
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

            let traceback_val = manifest.get("traceback")?;
            let traceback = if let Some(arr) = traceback_val.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                let tb_str = resolve_content_ref_async(traceback_val, blob_store_path).await?;
                serde_json::from_str::<Vec<String>>(&tb_str).ok()?
            };

            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Resolve a content reference from a blob manifest.
async fn resolve_content_ref_async(
    content_ref: &serde_json::Value,
    blob_store_path: &Option<PathBuf>,
) -> Option<String> {
    if let Some(inline) = content_ref.get("inline") {
        return inline.as_str().map(|s| s.to_string());
    }

    if let Some(blob_hash) = content_ref.get("blob").and_then(|v| v.as_str()) {
        if let Some(store_path) = blob_store_path {
            if blob_hash.len() >= 2 {
                let prefix = &blob_hash[..2];
                let rest = &blob_hash[2..];
                let blob_path = store_path.join(prefix).join(rest);

                if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                    return Some(contents);
                }
            }
        }
    }

    None
}
