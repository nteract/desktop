//! Async session for code execution.
//!
//! Provides an async interface for executing code via the daemon's kernel.
//! All methods return Python coroutines that can be awaited.

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use runtimed::notebook_sync_client::{
    NotebookBroadcastReceiver, NotebookSyncClient, NotebookSyncHandle, NotebookSyncReceiver,
};
use runtimed::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use crate::daemon_paths::{get_blob_paths_async, get_socket_path};
use crate::error::to_py_err;
use crate::event_stream::ExecutionEventStream;
use crate::output::{Cell, ExecutionResult, NotebookConnectionInfo, Output, SyncEnvironmentResult};
use crate::output_resolver;
use crate::subscription::EventSubscription;

use notebook_doc::metadata::NotebookMetadataSnapshot;

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
    peer_label: Option<String>,
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
    /// Connection info from daemon (for open_notebook/create_notebook)
    connection_info: Option<NotebookConnectionInfo>,
    /// Notebook path (for project file detection during kernel launch)
    notebook_path: Option<String>,
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
            connection_info: None,
            notebook_path: None,
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
    #[pyo3(signature = (notebook_id=None, peer_label=None))]
    fn new(notebook_id: Option<String>, peer_label: Option<String>) -> PyResult<Self> {
        let notebook_id =
            notebook_id.unwrap_or_else(|| format!("agent-session-{}", uuid::Uuid::new_v4()));

        Ok(Self {
            state: Arc::new(Mutex::new(AsyncSessionState::new())),
            notebook_id,
            peer_label,
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

    /// Get the connection info from daemon (for open_notebook/create_notebook).
    ///
    /// Returns None if not connected via open_notebook() or create_notebook().
    /// Returns a coroutine that resolves to Optional[NotebookConnectionInfo].
    fn connection_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let state = state.lock().await;
            Ok(state.connection_info.clone())
        })
    }

    /// Open an existing notebook file via the daemon.
    ///
    /// The daemon loads the file, derives the notebook_id from the canonical path,
    /// and returns connection info including trust status.
    ///
    /// Args:
    ///     path: Path to the .ipynb file.
    ///
    /// Returns:
    ///     A coroutine that resolves to a new AsyncSession connected to the opened notebook.
    ///
    /// Raises:
    ///     RuntimedError: If the file cannot be opened or parsed.
    #[staticmethod]
    #[pyo3(signature = (path, peer_label=None))]
    fn open_notebook(
        py: Python<'_>,
        path: String,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let path_buf = PathBuf::from(&path);
            let socket_path = get_socket_path();

            let (handle, sync_rx, broadcast_rx, _cells, _metadata, info) =
                NotebookSyncClient::connect_open_split(socket_path.clone(), path_buf, None)
                    .await
                    .map_err(to_py_err)?;

            // Check for error in response
            if let Some(error) = info.error {
                return Err(to_py_err(error));
            }

            let notebook_id = info.notebook_id.clone();
            let connection_info = NotebookConnectionInfo::from_protocol(info);
            let (blob_base_url, blob_store_path) = get_blob_paths_async(&socket_path).await;

            let state = AsyncSessionState {
                handle: Some(handle),
                sync_rx: Some(sync_rx),
                broadcast_rx: Some(broadcast_rx),
                kernel_started: false,
                env_source: None,
                blob_base_url,
                blob_store_path,
                connection_info: Some(connection_info),
                notebook_path: Some(path),
            };

            Ok(AsyncSession {
                state: Arc::new(Mutex::new(state)),
                notebook_id,
                peer_label,
            })
        })
    }

    /// Create a new notebook via the daemon.
    ///
    /// The daemon creates an empty notebook with one code cell and returns
    /// connection info with a generated UUID as the notebook_id.
    ///
    /// Args:
    ///     runtime: The kernel runtime type ("python" or "deno"). Defaults to "python".
    ///     working_dir: Optional working directory for project file detection.
    ///
    /// Returns:
    ///     A coroutine that resolves to a new AsyncSession connected to the created notebook.
    #[staticmethod]
    #[pyo3(signature = (runtime="python", working_dir=None, peer_label=None))]
    fn create_notebook<'py>(
        py: Python<'py>,
        runtime: &str,
        working_dir: Option<String>,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Validate working_dir if provided
        if let Some(ref wd) = working_dir {
            let path = std::path::Path::new(wd);
            if !path.exists() {
                return Err(pyo3::exceptions::PyFileNotFoundError::new_err(format!(
                    "working_dir does not exist: {}",
                    wd
                )));
            }
            if !path.is_dir() {
                return Err(pyo3::exceptions::PyNotADirectoryError::new_err(format!(
                    "working_dir is not a directory: {}",
                    wd
                )));
            }
        }

        let runtime = runtime.to_string();
        future_into_py(py, async move {
            let working_dir_str = working_dir.clone();
            let working_dir_buf = working_dir.map(PathBuf::from);
            let socket_path = get_socket_path();

            let (handle, sync_rx, broadcast_rx, _cells, _metadata, info) =
                NotebookSyncClient::connect_create_split(
                    socket_path.clone(),
                    runtime,
                    working_dir_buf,
                    None,
                    None,
                )
                .await
                .map_err(to_py_err)?;

            // Check for error in response
            if let Some(error) = info.error {
                return Err(to_py_err(error));
            }

            let notebook_id = info.notebook_id.clone();
            let connection_info = NotebookConnectionInfo::from_protocol(info);
            let (blob_base_url, blob_store_path) = get_blob_paths_async(&socket_path).await;

            let state = AsyncSessionState {
                handle: Some(handle),
                sync_rx: Some(sync_rx),
                broadcast_rx: Some(broadcast_rx),
                kernel_started: false,
                env_source: None,
                blob_base_url,
                blob_store_path,
                connection_info: Some(connection_info),
                notebook_path: working_dir_str,
            };

            Ok(AsyncSession {
                state: Arc::new(Mutex::new(state)),
                notebook_id,
                peer_label,
            })
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

            let socket_path = get_socket_path();

            let (handle, sync_rx, broadcast_rx, _cells, _notebook_path) =
                NotebookSyncClient::connect_split(socket_path.clone(), notebook_id)
                    .await
                    .map_err(to_py_err)?;

            let (blob_base_url, blob_store_path) = get_blob_paths_async(&socket_path).await;

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
    ///     notebook_path: Optional path to the notebook file on disk.
    ///         Used for project file detection (pyproject.toml, pixi.toml,
    ///         environment.yml) when env_source is "auto". If not provided,
    ///         uses the path from open_notebook() if available.
    ///
    /// If a kernel is already running for this session's notebook_id,
    /// this returns immediately without starting a new one.
    ///
    /// Returns a coroutine.
    #[pyo3(signature = (kernel_type="python", env_source="auto", notebook_path=None))]
    fn start_kernel<'py>(
        &self,
        py: Python<'py>,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let kernel_type = kernel_type.to_string();
        let env_source = env_source.to_string();
        let notebook_path = notebook_path.map(|s| s.to_string());

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

            // Use provided notebook_path or fall back to stored path from open_notebook()
            let resolved_path = notebook_path.or_else(|| state_guard.notebook_path.clone());

            let response = handle
                .send_request(NotebookRequest::LaunchKernel {
                    kernel_type,
                    env_source,
                    notebook_path: resolved_path,
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

            let cells = handle.get_cells();
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

    /// Append text to a cell's source in the automerge document.
    ///
    /// Unlike set_source() which replaces the entire text (using Myers diff
    /// internally), this directly inserts characters at the end of the source
    /// Text CRDT. This is ideal for streaming/agentic use cases where an
    /// external process is appending tokens incrementally — each append is
    /// a minimal CRDT operation that syncs efficiently to all connected clients.
    ///
    /// Args:
    ///     cell_id: The cell ID.
    ///     text: The text to append to the cell's source.
    ///
    /// Returns a coroutine.
    fn append_source<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        text: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let text = text.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle
                .append_source(&cell_id, &text)
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
            // Get snapshot and blob config while holding lock
            let (snapshot, blob_base_url, blob_store_path) = {
                let state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

                let blob_base_url = state_guard.blob_base_url.clone();
                let blob_store_path = state_guard.blob_store_path.clone();

                let cells = handle.get_cells();
                let snapshot = cells
                    .into_iter()
                    .find(|c| c.id == cell_id)
                    .ok_or_else(|| to_py_err(format!("Cell not found: {}", cell_id)))?;

                (snapshot, blob_base_url, blob_store_path)
            }; // Lock released here

            // Resolve outputs outside the lock
            let outputs = output_resolver::resolve_cell_outputs(
                &snapshot.outputs,
                &blob_base_url,
                &blob_store_path,
            )
            .await;

            Ok(Cell::from_snapshot_with_outputs(snapshot, outputs))
        })
    }

    /// Get all cells from the automerge document.
    ///
    /// Returns a coroutine that resolves to List[Cell].
    fn get_cells<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            // Get snapshots and blob config while holding lock
            let (snapshots, blob_base_url, blob_store_path) = {
                let state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

                let blob_base_url = state_guard.blob_base_url.clone();
                let blob_store_path = state_guard.blob_store_path.clone();

                let snapshots = handle.get_cells();
                (snapshots, blob_base_url, blob_store_path)
            }; // Lock released here

            // Resolve outputs for all cells outside the lock
            let mut cells = Vec::with_capacity(snapshots.len());
            for snapshot in snapshots {
                let outputs = output_resolver::resolve_cell_outputs(
                    &snapshot.outputs,
                    &blob_base_url,
                    &blob_store_path,
                )
                .await;
                cells.push(Cell::from_snapshot_with_outputs(snapshot, outputs));
            }
            Ok(cells)
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

    /// Move a cell to a new position in the notebook.
    ///
    /// Updates the cell's fractional index position field. No delete/re-insert —
    /// the cell object is preserved in the Automerge document.
    ///
    /// Args:
    ///     cell_id: The cell ID to move.
    ///     after_cell_id: Place the cell after this cell ID. None means move to the start.
    ///
    /// Returns:
    ///     A coroutine that resolves to the new fractional position string.
    #[pyo3(signature = (cell_id, after_cell_id=None))]
    fn move_cell<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let after_cell_id = after_cell_id.map(|s| s.to_string());

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle
                .move_cell(&cell_id, after_cell_id.as_deref())
                .await
                .map_err(to_py_err)
        })
    }

    /// Send a cursor position as presence data to other connected peers.
    ///
    /// The daemon relays this to all other peers in the notebook room.
    ///
    /// Args:
    ///     cell_id: The cell the cursor is in.
    ///     line: Line number (0-based).
    ///     column: Column number (0-based).
    ///
    /// Returns a coroutine.
    fn set_cursor<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        line: u32,
        column: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let data = notebook_doc::presence::encode_cursor_update_labeled(
            "local",
            self.peer_label.as_deref(),
            &notebook_doc::presence::CursorPosition {
                cell_id: cell_id.to_string(),
                line,
                column,
            },
        );

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;
            handle.send_presence(data).await.map_err(to_py_err)
        })
    }

    /// Send a selection range as presence data to other connected peers.
    ///
    /// Args:
    ///     cell_id: The cell the selection is in.
    ///     anchor_line: Selection anchor line (0-based).
    ///     anchor_col: Selection anchor column (0-based).
    ///     head_line: Selection head line (0-based).
    ///     head_col: Selection head column (0-based).
    ///
    /// Returns a coroutine.
    fn set_selection<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        anchor_line: u32,
        anchor_col: u32,
        head_line: u32,
        head_col: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let data = notebook_doc::presence::encode_selection_update_labeled(
            "local",
            self.peer_label.as_deref(),
            &notebook_doc::presence::SelectionRange {
                cell_id: cell_id.to_string(),
                anchor_line,
                anchor_col,
                head_line,
                head_col,
            },
        );

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;
            handle.send_presence(data).await.map_err(to_py_err)
        })
    }

    /// Save the notebook to a .ipynb file.
    ///
    /// Reads cells and metadata from the synced Automerge document, resolves
    /// output manifests from the blob store, and writes standard nbformat v4 JSON.
    ///
    /// Args:
    ///     path: Optional target path for the notebook file. If it doesn't end
    ///           with .ipynb, the extension will be appended. If None, saves to
    ///           the notebook's original file path (the notebook_id).
    ///
    /// Returns:
    ///     A coroutine that resolves to the absolute path where the file was written.
    ///
    /// Raises:
    ///     RuntimedError: If not connected or write fails.
    #[pyo3(signature = (path=None))]
    fn save<'py>(&self, py: Python<'py>, path: Option<&str>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let path = path.map(|s| s.to_string());

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::SaveNotebook {
                    format_cells: false,
                    path,
                })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::NotebookSaved { path } => Ok(path),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
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
    ///
    /// Returns a coroutine.
    fn set_metadata<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        value: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let key = key.to_string();
        let value = value.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
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
    /// Returns a coroutine that resolves to the metadata value (str) or None.
    fn get_metadata<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let key = key.to_string();

        future_into_py(py, async move {
            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            handle.get_metadata(&key).await.map_err(to_py_err)
        })
    }

    // =========================================================================
    // Dependency Management (convenience methods for notebook_metadata)
    // =========================================================================

    /// Get current UV dependencies.
    ///
    /// Returns a coroutine that resolves to list of UV dependency specifiers.
    fn get_uv_dependencies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let snapshot = get_notebook_metadata_async(&state).await?;
            Ok(snapshot
                .runt
                .uv
                .map(|uv| uv.dependencies)
                .unwrap_or_default())
        })
    }

    /// Add a UV dependency to the notebook.
    ///
    /// Deduplicates by package name (case-insensitive): if a dependency with the
    /// same package name already exists, it is replaced with the new specifier.
    ///
    /// Args:
    ///     package: PEP 508 dependency specifier (e.g., "pandas>=2.0", "requests").
    ///
    /// Returns a coroutine that resolves to None. Callers should use
    /// `get_uv_dependencies()` to read current state.
    fn add_uv_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();

        future_into_py(py, async move {
            let mut snapshot = get_notebook_metadata_async(&state).await?;
            snapshot.add_uv_dependency(&package);
            set_notebook_metadata_async(&state, &snapshot).await?;
            Ok(())
        })
    }

    /// Remove a UV dependency by package name (case-insensitive, version-agnostic).
    ///
    /// Args:
    ///     package: Package name to remove (e.g., "pandas"). Version specifiers are ignored.
    ///
    /// Returns a coroutine that resolves to bool indicating if a dependency was removed.
    fn remove_uv_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();

        future_into_py(py, async move {
            let mut snapshot = get_notebook_metadata_async(&state).await?;
            let removed = snapshot.remove_uv_dependency(&package);
            if removed {
                set_notebook_metadata_async(&state, &snapshot).await?;
            }
            Ok(removed)
        })
    }

    /// Get current Conda dependencies.
    ///
    /// Returns a coroutine that resolves to list of Conda package names.
    fn get_conda_dependencies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let snapshot = get_notebook_metadata_async(&state).await?;
            Ok(snapshot
                .runt
                .conda
                .map(|c| c.dependencies)
                .unwrap_or_default())
        })
    }

    /// Add a Conda dependency to the notebook.
    ///
    /// Deduplicates by package name (case-insensitive): if a dependency with the
    /// same package name already exists, it is replaced with the new specifier.
    ///
    /// Args:
    ///     package: Conda package specifier (e.g., "numpy", "scipy>=1.0").
    ///
    /// Returns a coroutine that resolves to None. Callers should use
    /// `get_conda_dependencies()` to read current state.
    fn add_conda_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();

        future_into_py(py, async move {
            let mut snapshot = get_notebook_metadata_async(&state).await?;
            snapshot.add_conda_dependency(&package);
            set_notebook_metadata_async(&state, &snapshot).await?;
            Ok(())
        })
    }

    /// Remove a Conda dependency by package name (case-insensitive, version-agnostic).
    ///
    /// Args:
    ///     package: Package name to remove (e.g., "numpy"). Version specifiers are ignored.
    ///
    /// Returns a coroutine that resolves to bool indicating if a dependency was removed.
    fn remove_conda_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();

        future_into_py(py, async move {
            let mut snapshot = get_notebook_metadata_async(&state).await?;
            let removed = snapshot.remove_conda_dependency(&package);
            if removed {
                set_notebook_metadata_async(&state, &snapshot).await?;
            }
            Ok(removed)
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
                    } else {
                        // Already connected - drop the lock before starting kernel
                        drop(state_guard);
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

            // Confirm the daemon has merged our latest changes before executing.
            // The daemon reads cell source from its own Automerge doc, so it must
            // have the cell before we can reference it by ID.
            handle.confirm_sync().await.map_err(to_py_err)?;

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

    /// Stream execution events for a cell as an async iterator.
    ///
    /// Unlike execute_cell() which blocks until completion and returns all
    /// outputs at once, this returns an async iterator that yields ExecutionEvent
    /// objects as they arrive from the kernel, enabling real-time processing.
    ///
    /// Example:
    ///     ```python
    ///     async for event in await session.stream_execute(cell_id):
    ///         if event.event_type == "output":
    ///             print(event.output.text)  # Process output immediately
    ///     ```
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///     timeout_secs: Maximum time to wait for each event (default: 60).
    ///     signal_only: If True, output events contain only output_index, not
    ///         the full output data. Use get_cell() to fetch the data.
    ///
    /// Returns a coroutine that resolves to ExecutionEventStream (async iterator).
    #[pyo3(signature = (cell_id, timeout_secs=60.0, signal_only=false))]
    fn stream_execute<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        timeout_secs: f64,
        signal_only: bool,
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
                    } else {
                        // Already connected - drop the lock before starting kernel
                        drop(state_guard);
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

            // Queue execution and get broadcast receiver for streaming
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            // Confirm the daemon has merged our latest changes before executing.
            handle.confirm_sync().await.map_err(to_py_err)?;

            // Queue the cell for execution
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

            // Get a resubscribed broadcast receiver for this stream
            let stream_broadcast_rx = state_guard
                .broadcast_rx
                .as_ref()
                .ok_or_else(|| to_py_err("No broadcast receiver"))?
                .resubscribe();

            let blob_base_url = state_guard.blob_base_url.clone();
            let blob_store_path = state_guard.blob_store_path.clone();

            drop(state_guard);

            // Return the async iterator
            Ok(ExecutionEventStream::new(
                stream_broadcast_rx,
                cell_id,
                timeout_secs,
                blob_base_url,
                blob_store_path,
                signal_only,
            ))
        })
    }

    /// Subscribe to notebook broadcast events independently of execution.
    ///
    /// Returns an async iterator that yields all broadcast events from the
    /// notebook, optionally filtered by cell IDs and event types. This
    /// enables reactive patterns for agents that want to respond to any
    /// document activity (including executions from other clients).
    ///
    /// Example:
    ///     ```python
    ///     # Subscribe to all events
    ///     async for event in session.subscribe():
    ///         print(f"Got: {event.event_type}")
    ///
    ///     # Subscribe with filters
    ///     async for event in session.subscribe(event_types=["output", "done"]):
    ///         if event.event_type == "output":
    ///             print(event.output.text)
    ///     ```
    ///
    /// Args:
    ///     cell_ids: Optional list of cell IDs to filter events.
    ///     event_types: Optional list of event types to filter. Valid types:
    ///         "execution_started", "output", "done", "error", "kernel_status".
    ///
    /// Returns an EventSubscription async iterator.
    #[pyo3(signature = (cell_ids=None, event_types=None))]
    fn subscribe(
        &self,
        cell_ids: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
    ) -> PyResult<EventSubscription> {
        // Use a temporary runtime to access the state synchronously
        let runtime = tokio::runtime::Runtime::new().map_err(to_py_err)?;
        let state = runtime.block_on(self.state.lock());

        let broadcast_rx = state
            .broadcast_rx
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected - call connect() or start_kernel() first"))?
            .resubscribe();

        let blob_base_url = state.blob_base_url.clone();
        let blob_store_path = state.blob_store_path.clone();

        drop(state);

        Ok(EventSubscription::new(
            broadcast_rx,
            cell_ids,
            event_types,
            blob_base_url,
            blob_store_path,
        ))
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
                let cells = handle.get_cells();
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

                // Confirm the daemon has merged our latest changes before executing.
                handle.confirm_sync().await.map_err(to_py_err)?;

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

            // Confirm the daemon has merged our latest changes before executing.
            handle.confirm_sync().await.map_err(to_py_err)?;

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

    /// Restart the kernel.
    ///
    /// Shuts down the current kernel and starts a new one. This is useful after
    /// modifying dependencies to apply the changes.
    ///
    /// The new kernel will use env_source="auto" to pick up any inline
    /// dependencies from the notebook metadata.
    ///
    /// Args:
    ///     wait_for_ready: If True (default), wait for kernel to be idle.
    ///
    /// Note: This currently does shutdown + start. A daemon-side RestartKernel
    /// command would be cleaner but doesn't exist yet.
    ///
    /// Returns a coroutine.
    #[pyo3(signature = (wait_for_ready=true))]
    fn restart_kernel<'py>(
        &self,
        py: Python<'py>,
        wait_for_ready: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();

        future_into_py(py, async move {
            // TODO: Consider adding NotebookRequest::RestartKernel to the daemon
            // Shutdown existing kernel
            {
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
                    NotebookResponse::KernelShuttingDown {} | NotebookResponse::NoKernel {} => {
                        state_guard.kernel_started = false;
                        state_guard.env_source = None;
                    }
                    NotebookResponse::Error { error } => return Err(to_py_err(error)),
                    _ => {}
                }
            }

            // Start new kernel with auto env detection
            {
                let mut state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

                let response = handle
                    .send_request(NotebookRequest::LaunchKernel {
                        kernel_type: "python".to_string(),
                        env_source: "auto".to_string(),
                        notebook_path: Some(notebook_id.clone()),
                    })
                    .await
                    .map_err(to_py_err)?;

                match response {
                    NotebookResponse::KernelLaunched { env_source, .. }
                    | NotebookResponse::KernelAlreadyRunning { env_source, .. } => {
                        state_guard.kernel_started = true;
                        state_guard.env_source = Some(env_source);
                    }
                    NotebookResponse::Error { error } => return Err(to_py_err(error)),
                    other => return Err(to_py_err(format!("Unexpected response: {:?}", other))),
                }
            }

            // Wait for kernel ready if requested
            if wait_for_ready {
                let mut state_guard = state.lock().await;
                if let Some(rx) = state_guard.broadcast_rx.as_mut() {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    while std::time::Instant::now() < deadline {
                        match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                            .await
                        {
                            Ok(Some(NotebookBroadcast::KernelStatus { status, .. }))
                                if status == "idle" =>
                            {
                                return Ok(());
                            }
                            Ok(Some(_)) => continue,
                            Ok(None) => return Err(to_py_err("Broadcast channel closed")),
                            Err(_) => continue,
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// Sync environment with current metadata (hot-install new packages).
    ///
    /// This attempts to install new packages without restarting the kernel.
    /// Only supported for UV inline dependencies with additions only.
    ///
    /// For removals, conda dependencies, or other cases, this will return
    /// an error with needs_restart=True indicating a kernel restart is required.
    ///
    /// Returns a coroutine that resolves to SyncEnvironmentResult.
    fn sync_environment<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let response = {
                let state_guard = state.lock().await;
                let handle = state_guard
                    .handle
                    .as_ref()
                    .ok_or_else(|| to_py_err("Not connected"))?;

                handle
                    .send_request(NotebookRequest::SyncEnvironment {})
                    .await
                    .map_err(to_py_err)?
            };

            match response {
                NotebookResponse::SyncEnvironmentStarted { packages } => {
                    // Wait for completion
                    let mut state_guard = state.lock().await;
                    if let Some(rx) = state_guard.broadcast_rx.as_mut() {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(300);
                        while std::time::Instant::now() < deadline {
                            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                                .await
                            {
                                Ok(Some(NotebookBroadcast::EnvSyncState {
                                    in_sync: true, ..
                                })) => {
                                    return Ok(SyncEnvironmentResult {
                                        success: true,
                                        synced_packages: packages,
                                        error: None,
                                        needs_restart: false,
                                    });
                                }
                                Ok(Some(_)) => continue,
                                Ok(None) => break,
                                Err(_) => continue,
                            }
                        }
                    }
                    // Assume success if we got SyncEnvironmentStarted
                    Ok(SyncEnvironmentResult {
                        success: true,
                        synced_packages: packages,
                        error: None,
                        needs_restart: false,
                    })
                }
                NotebookResponse::SyncEnvironmentComplete { synced_packages } => {
                    Ok(SyncEnvironmentResult {
                        success: true,
                        synced_packages,
                        error: None,
                        needs_restart: false,
                    })
                }
                NotebookResponse::SyncEnvironmentFailed {
                    error,
                    needs_restart,
                } => Ok(SyncEnvironmentResult {
                    success: false,
                    synced_packages: vec![],
                    error: Some(error),
                    needs_restart,
                }),
                NotebookResponse::NoKernel {} => Ok(SyncEnvironmentResult {
                    success: false,
                    synced_packages: vec![],
                    error: Some("No kernel running".to_string()),
                    needs_restart: true,
                }),
                NotebookResponse::Error { error } => Ok(SyncEnvironmentResult {
                    success: false,
                    synced_packages: vec![],
                    error: Some(error),
                    needs_restart: true,
                }),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    // =========================================================================
    // Kernel Introspection (completion, history, queue state)
    // =========================================================================

    /// Get code completions at a cursor position.
    ///
    /// The kernel provides completions based on the current code context,
    /// including DataFrame columns, object methods, variable names, etc.
    ///
    /// Args:
    ///     code: The code to complete.
    ///     cursor_pos: Cursor position in the code (byte offset).
    ///
    /// Returns a coroutine that resolves to CompletionResult with items,
    /// cursor_start, and cursor_end.
    #[pyo3(signature = (code, cursor_pos))]
    fn complete<'py>(
        &self,
        py: Python<'py>,
        code: String,
        cursor_pos: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::Complete { code, cursor_pos })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::CompletionResult {
                    items,
                    cursor_start,
                    cursor_end,
                } => Ok(crate::output::CompletionResult {
                    items: items
                        .into_iter()
                        .map(crate::output::CompletionItem::from_protocol)
                        .collect(),
                    cursor_start,
                    cursor_end,
                }),
                NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Get the current execution queue state.
    ///
    /// Returns a coroutine that resolves to QueueState with executing
    /// (cell_id or None) and queued (list of cell_ids).
    fn get_queue_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::GetQueueState {})
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::QueueState { executing, queued } => {
                    Ok(crate::output::QueueState { executing, queued })
                }
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Clear outputs for a cell.
    ///
    /// Removes all outputs and resets the execution count. Useful before
    /// re-executing a cell for a fresh run.
    ///
    /// Args:
    ///     cell_id: The cell ID to clear outputs for.
    ///
    /// Returns a coroutine.
    fn clear_outputs<'py>(&self, py: Python<'py>, cell_id: String) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::ClearOutputs { cell_id })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::OutputsCleared { .. } => Ok(()),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Run all code cells in the notebook.
    ///
    /// Queues all code cells (in document order) for execution. The daemon
    /// reads cell sources from the automerge document and executes them
    /// sequentially.
    ///
    /// Returns a coroutine that resolves to the number of cells queued.
    fn run_all_cells<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            // Check if kernel needs to be started
            let needs_kernel = {
                let state_guard = state.lock().await;
                !state_guard.kernel_started
            };

            if needs_kernel {
                // Start kernel (simplified - in practice you'd want to call the full start_kernel logic)
                let mut state_guard = state.lock().await;
                if let Some(handle) = state_guard.handle.as_ref() {
                    let response = handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: "python".to_string(),
                            env_source: "auto".to_string(),
                            notebook_path: None,
                        })
                        .await
                        .map_err(to_py_err)?;

                    match response {
                        NotebookResponse::KernelLaunched { env_source, .. }
                        | NotebookResponse::KernelAlreadyRunning { env_source, .. } => {
                            state_guard.kernel_started = true;
                            state_guard.env_source = Some(env_source);
                        }
                        NotebookResponse::Error { error } => return Err(to_py_err(error)),
                        _ => {}
                    }
                }
            }

            let state_guard = state.lock().await;
            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::RunAllCells {})
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::AllCellsQueued { count } => Ok(count),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Search the kernel's input history.
    ///
    /// Returns executed code from the kernel's history, optionally filtered
    /// by a glob pattern.
    ///
    /// Args:
    ///     pattern: Optional glob pattern to filter history (e.g., "*pandas*").
    ///     n: Maximum number of entries to return (default: 100).
    ///     unique: If True, deduplicate entries (default: True).
    ///
    /// Returns a coroutine that resolves to a list of HistoryEntry objects.
    #[pyo3(signature = (pattern=None, n=100, unique=true))]
    fn get_history<'py>(
        &self,
        py: Python<'py>,
        pattern: Option<String>,
        n: i32,
        unique: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let state_guard = state.lock().await;

            let handle = state_guard
                .handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?;

            let response = handle
                .send_request(NotebookRequest::GetHistory { pattern, n, unique })
                .await
                .map_err(to_py_err)?;

            match response {
                NotebookResponse::HistoryResult { entries } => Ok(entries
                    .into_iter()
                    .map(crate::output::HistoryEntry::from_protocol)
                    .collect::<Vec<_>>()),
                NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
                NotebookResponse::Error { error } => Err(to_py_err(error)),
                other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
            }
        })
    }

    /// Close the session and shutdown the kernel if running.
    ///
    /// Close the session.
    ///
    /// Does NOT shutdown the kernel - the daemon handles kernel lifecycle
    /// based on peer count. When all peers disconnect, the daemon will
    /// clean up the kernel. Use shutdown_kernel() explicitly if you need
    /// to stop the kernel.
    ///
    /// Returns a coroutine.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move { Ok(()) })
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
    ///
    /// Does NOT shutdown the kernel - the daemon handles kernel lifecycle
    /// based on peer count. When all peers disconnect, the daemon will
    /// clean up the kernel. This prevents killing kernels that desktop
    /// app users may still be using.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move { Ok(false) }) // Don't suppress exceptions
    }
}

// =========================================================================
// Helper functions (outside impl block for async use)
// =========================================================================

/// Wait for execution to complete, then read outputs from the Automerge doc.
///
/// Uses the broadcast stream only as a signal for when execution is done.
/// The Automerge document is the source of truth for cell outputs — this
/// sidesteps the Jupyter shell/iopub race condition entirely since the
/// daemon writes all outputs to the doc before signaling ExecutionDone.
async fn collect_outputs_async(
    state: &Arc<Mutex<AsyncSessionState>>,
    cell_id: &str,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
) -> PyResult<ExecutionResult> {
    let mut kernel_error: Option<String> = None;

    // Phase 1: Wait for ExecutionDone or KernelError signal via broadcast.
    // We don't accumulate outputs here — the Automerge doc is the source of truth.
    loop {
        let mut state_guard = state.lock().await;

        let broadcast_rx = state_guard
            .broadcast_rx
            .as_mut()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let broadcast =
            tokio::time::timeout(std::time::Duration::from_millis(100), broadcast_rx.recv()).await;

        match broadcast {
            Ok(Some(msg)) => {
                drop(state_guard);

                match msg {
                    NotebookBroadcast::ExecutionDone {
                        cell_id: msg_cell_id,
                    } => {
                        if msg_cell_id == cell_id {
                            log::debug!("[async_session] ExecutionDone received for {}", cell_id);
                            break;
                        }
                    }
                    NotebookBroadcast::KernelError { error } => {
                        log::debug!("[async_session] KernelError: {}", error);
                        kernel_error = Some(error);
                        break;
                    }
                    _ => {
                        // Ignore all other broadcasts — doc has the data
                    }
                }
            }
            Ok(None) => {
                return Err(to_py_err("Broadcast channel closed"));
            }
            Err(_) => {
                // Poll timeout — keep waiting for signal
            }
        }
    }

    // KernelError means the kernel/daemon died — return immediately
    // without trying to read from the doc (handle may be gone).
    if let Some(error) = kernel_error {
        return Ok(ExecutionResult {
            cell_id: cell_id.to_string(),
            outputs: vec![Output::error("KernelError", &error, vec![])],
            success: false,
            execution_count: None,
        });
    }

    // Phase 2: Read canonical cell state from the Automerge doc.
    // The daemon writes outputs to the doc as they arrive from the kernel,
    // but ExecutionDone can arrive via broadcast before the final Automerge
    // sync frame. A confirm_sync round-trip ensures our local doc replica
    // has all the outputs before we read.
    let (snapshot, blob_base_url, blob_store_path) = {
        let state_guard = state.lock().await;
        let handle = state_guard
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle.confirm_sync().await.map_err(to_py_err)?;

        let cells = handle.get_cells();
        let snapshot = cells.into_iter().find(|c| c.id == cell_id).ok_or_else(|| {
            to_py_err(format!(
                "Cell not found in doc after execution: {}",
                cell_id
            ))
        })?;

        (snapshot, blob_base_url, blob_store_path)
    }; // state lock dropped before async I/O

    let execution_count = snapshot.execution_count.parse::<i64>().ok();

    // Resolve outputs from doc snapshot (blob hashes → Output objects)
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

/// Get the notebook metadata snapshot asynchronously.
async fn get_notebook_metadata_async(
    state: &Arc<Mutex<AsyncSessionState>>,
) -> PyResult<NotebookMetadataSnapshot> {
    let state_guard = state.lock().await;
    let handle = state_guard
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    Ok(handle.get_notebook_metadata().unwrap_or_default())
}

/// Set the notebook metadata snapshot asynchronously.
async fn set_notebook_metadata_async(
    state: &Arc<Mutex<AsyncSessionState>>,
    snapshot: &NotebookMetadataSnapshot,
) -> PyResult<()> {
    let json_str = serde_json::to_string(snapshot)
        .map_err(|e| to_py_err(format!("Failed to serialize metadata: {}", e)))?;

    let state_guard = state.lock().await;
    let handle = state_guard
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle
        .set_metadata("notebook_metadata", &json_str)
        .await
        .map_err(to_py_err)
}
