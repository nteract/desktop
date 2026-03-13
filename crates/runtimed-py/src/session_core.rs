//! Shared async core for Session and AsyncSession.
//!
//! All business logic lives here as free async functions operating on
//! `Arc<Mutex<SessionState>>`. The sync `Session` calls these via
//! `runtime.block_on()`, and `AsyncSession` calls them via
//! `future_into_py()`. This eliminates the duplication that previously
//! existed between session.rs and async_session.rs.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use runtimed::notebook_sync_client::{
    NotebookBroadcastReceiver, NotebookSyncClient, NotebookSyncHandle, NotebookSyncReceiver,
};
use runtimed::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use notebook_doc::metadata::NotebookMetadataSnapshot;

use crate::daemon_paths::get_socket_path;
use crate::error::to_py_err;
use crate::output::{
    Cell, CompletionItem, CompletionResult, ExecutionResult, HistoryEntry, NotebookConnectionInfo,
    Output, QueueState, SyncEnvironmentResult,
};
use crate::output_resolver;

use pyo3::prelude::*;

// =========================================================================
// Shared state
// =========================================================================

/// Internal state shared between Session and AsyncSession.
///
/// Both wrappers hold `Arc<Mutex<SessionState>>` and delegate all
/// async operations to the free functions in this module.
pub(crate) struct SessionState {
    pub handle: Option<NotebookSyncHandle>,
    /// Keep the sync receiver alive so the sync task doesn't exit
    #[allow(dead_code)]
    pub sync_rx: Option<NotebookSyncReceiver>,
    pub broadcast_rx: Option<NotebookBroadcastReceiver>,
    pub kernel_started: bool,
    pub env_source: Option<String>,
    /// Base URL for blob server (for resolving blob hashes)
    pub blob_base_url: Option<String>,
    /// Path to blob store directory (fallback for direct disk access)
    pub blob_store_path: Option<PathBuf>,
    /// Connection info from daemon (for open_notebook/create_notebook)
    pub connection_info: Option<NotebookConnectionInfo>,
    /// Notebook path (for project file detection during kernel launch)
    pub notebook_path: Option<String>,
}

impl SessionState {
    pub fn new() -> Self {
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

// =========================================================================
// Connection
// =========================================================================

/// Connect to the daemon if not already connected.
///
/// Populates the state with handle, sync_rx, broadcast_rx, and blob paths.
pub(crate) async fn connect(state: &Arc<Mutex<SessionState>>, notebook_id: &str) -> PyResult<()> {
    let mut st = state.lock().await;
    if st.handle.is_some() {
        return Ok(());
    }

    let socket_path = get_socket_path();

    let (handle, sync_rx, broadcast_rx, _cells, _notebook_path) =
        NotebookSyncClient::connect_split(socket_path.clone(), notebook_id.to_string())
            .await
            .map_err(to_py_err)?;

    // Resolve blob paths from daemon info
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    st.handle = Some(handle);
    st.sync_rx = Some(sync_rx);
    st.broadcast_rx = Some(broadcast_rx);
    st.blob_base_url = blob_base_url;
    st.blob_store_path = blob_store_path;

    Ok(())
}

/// Connect and open an existing notebook file.
///
/// Returns (notebook_id, populated SessionState, NotebookConnectionInfo).
pub(crate) async fn connect_open(
    socket_path: PathBuf,
    path: &str,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
    let (handle, sync_rx, broadcast_rx, _cells, _metadata, info) =
        NotebookSyncClient::connect_open_split(
            socket_path.clone(),
            PathBuf::from(path),
            None, // pipe_channel
        )
        .await
        .map_err(to_py_err)?;

    if let Some(ref error) = info.error {
        return Err(to_py_err(error));
    }

    let notebook_id = info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;
    let connection_info = NotebookConnectionInfo::from_protocol(info);

    let state = SessionState {
        handle: Some(handle),
        sync_rx: Some(sync_rx),
        broadcast_rx: Some(broadcast_rx),
        kernel_started: false,
        env_source: None,
        blob_base_url,
        blob_store_path,
        connection_info: Some(connection_info.clone()),
        notebook_path: Some(path.to_string()),
    };

    Ok((notebook_id, state, connection_info))
}

/// Connect and create a new notebook.
///
/// Returns (notebook_id, populated SessionState, NotebookConnectionInfo).
pub(crate) async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
    let (handle, sync_rx, broadcast_rx, _cells, _metadata, info) =
        NotebookSyncClient::connect_create_split(
            socket_path.clone(),
            runtime.to_string(),
            working_dir.clone(),
            None, // pipe_channel
            None, // initial_metadata
        )
        .await
        .map_err(to_py_err)?;

    if let Some(ref error) = info.error {
        return Err(to_py_err(error));
    }

    let notebook_id = info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;
    let connection_info = NotebookConnectionInfo::from_protocol(info);

    let state = SessionState {
        handle: Some(handle),
        sync_rx: Some(sync_rx),
        broadcast_rx: Some(broadcast_rx),
        kernel_started: false,
        env_source: None,
        blob_base_url,
        blob_store_path,
        connection_info: Some(connection_info.clone()),
        notebook_path: working_dir.map(|p| p.to_string_lossy().to_string()),
    };

    Ok((notebook_id, state, connection_info))
}

// =========================================================================
// Kernel lifecycle
// =========================================================================

/// Start a kernel in the daemon.
pub(crate) async fn start_kernel(
    state: &Arc<Mutex<SessionState>>,
    kernel_type: &str,
    env_source: &str,
    notebook_path: Option<&str>,
) -> PyResult<()> {
    let mut st = state.lock().await;

    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    // Resolve notebook path: explicit arg > stored state > None
    let resolved_path = notebook_path
        .map(|p| p.to_string())
        .or_else(|| st.notebook_path.clone());

    let response = handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type: kernel_type.to_string(),
            env_source: env_source.to_string(),
            notebook_path: resolved_path,
        })
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::KernelLaunched {
            env_source: actual_env,
            ..
        } => {
            st.kernel_started = true;
            st.env_source = Some(actual_env);
            Ok(())
        }
        NotebookResponse::KernelAlreadyRunning {
            env_source: actual_env,
            ..
        } => {
            st.kernel_started = true;
            st.env_source = Some(actual_env);
            Ok(())
        }
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Shutdown the kernel.
pub(crate) async fn shutdown_kernel(state: &Arc<Mutex<SessionState>>) -> PyResult<()> {
    let mut st = state.lock().await;

    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::ShutdownKernel {})
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::KernelShuttingDown {} | NotebookResponse::NoKernel {} => {
            st.kernel_started = false;
            st.env_source = None;
            Ok(())
        }
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Restart the kernel with auto environment detection.
pub(crate) async fn restart_kernel(
    state: &Arc<Mutex<SessionState>>,
    wait_for_ready: bool,
) -> PyResult<()> {
    // Shutdown
    {
        let mut st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let response = handle
            .send_request(NotebookRequest::ShutdownKernel {})
            .await
            .map_err(to_py_err)?;

        match response {
            NotebookResponse::KernelShuttingDown {} | NotebookResponse::NoKernel {} => {
                st.kernel_started = false;
                st.env_source = None;
            }
            NotebookResponse::Error { error } => return Err(to_py_err(error)),
            _ => {}
        }
    }

    // Start with auto env detection, passing stored notebook_path
    {
        let mut st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let resolved_path = st.notebook_path.clone();

        let response = handle
            .send_request(NotebookRequest::LaunchKernel {
                kernel_type: "python".to_string(),
                env_source: "auto".to_string(),
                notebook_path: resolved_path,
            })
            .await
            .map_err(to_py_err)?;

        match response {
            NotebookResponse::KernelLaunched {
                env_source: actual_env,
                ..
            }
            | NotebookResponse::KernelAlreadyRunning {
                env_source: actual_env,
                ..
            } => {
                st.kernel_started = true;
                st.env_source = Some(actual_env);
            }
            NotebookResponse::Error { error } => return Err(to_py_err(error)),
            other => return Err(to_py_err(format!("Unexpected response: {:?}", other))),
        }
    }

    // Wait for kernel ready
    if wait_for_ready {
        let mut st = state.lock().await;
        if let Some(rx) = st.broadcast_rx.as_mut() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
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
}

/// Interrupt the currently executing cell.
pub(crate) async fn interrupt(state: &Arc<Mutex<SessionState>>) -> PyResult<()> {
    let st = state.lock().await;

    let handle = st
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
}

// =========================================================================
// Cell operations
// =========================================================================

/// Create a new cell with source (atomic operation).
pub(crate) async fn create_cell(
    state: &Arc<Mutex<SessionState>>,
    source: &str,
    cell_type: &str,
    index: Option<usize>,
) -> PyResult<String> {
    let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let cells = handle.get_cells();
    let insert_index = index.unwrap_or(cells.len());

    handle
        .add_cell_with_source(insert_index, &cell_id, cell_type, source)
        .await
        .map_err(to_py_err)?;

    Ok(cell_id)
}

/// Update a cell's source.
pub(crate) async fn set_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    source: &str,
) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle
        .update_source(cell_id, source)
        .await
        .map_err(to_py_err)
}

/// Append text to a cell's source.
pub(crate) async fn append_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    text: &str,
) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.append_source(cell_id, text).await.map_err(to_py_err)
}

/// Get a single cell by ID, with resolved outputs.
pub(crate) async fn get_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<Cell> {
    let (snapshot, blob_base_url, blob_store_path) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let blob_base_url = st.blob_base_url.clone();
        let blob_store_path = st.blob_store_path.clone();

        let cells = handle.get_cells();
        let snapshot = cells
            .into_iter()
            .find(|c| c.id == cell_id)
            .ok_or_else(|| to_py_err(format!("Cell not found: {}", cell_id)))?;

        (snapshot, blob_base_url, blob_store_path)
    };

    let outputs =
        output_resolver::resolve_cell_outputs(&snapshot.outputs, &blob_base_url, &blob_store_path)
            .await;

    Ok(Cell::from_snapshot_with_outputs(snapshot, outputs))
}

/// Get all cells with resolved outputs.
pub(crate) async fn get_cells(state: &Arc<Mutex<SessionState>>) -> PyResult<Vec<Cell>> {
    let (snapshots, blob_base_url, blob_store_path) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let blob_base_url = st.blob_base_url.clone();
        let blob_store_path = st.blob_store_path.clone();
        let snapshots = handle.get_cells();

        (snapshots, blob_base_url, blob_store_path)
    };

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
}

/// Delete a cell.
pub(crate) async fn delete_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.delete_cell(cell_id).await.map_err(to_py_err)
}

/// Move a cell to a new position.
pub(crate) async fn move_cell(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    after_cell_id: Option<&str>,
) -> PyResult<String> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle
        .move_cell(cell_id, after_cell_id)
        .await
        .map_err(to_py_err)?;

    Ok(cell_id.to_string())
}

/// Clear a cell's outputs.
pub(crate) async fn clear_outputs(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::ClearOutputs {
            cell_id: cell_id.to_string(),
        })
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::OutputsCleared { .. } => Ok(()),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

// =========================================================================
// Execution
// =========================================================================

/// Execute a cell and return the result.
///
/// The entire lifecycle (confirm_sync, send_request, collect_outputs)
/// is wrapped in a single timeout.
pub(crate) async fn execute_cell(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
    cell_id: &str,
    timeout_secs: f64,
) -> PyResult<ExecutionResult> {
    // Auto-start kernel if not running
    {
        let st = state.lock().await;
        if !st.kernel_started {
            drop(st);
            ensure_kernel_started(state, notebook_id).await?;
        }
    }

    let timeout = std::time::Duration::from_secs_f64(timeout_secs);
    let result = tokio::time::timeout(timeout, async {
        let st = state.lock().await;

        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let blob_base_url = st.blob_base_url.clone();
        let blob_store_path = st.blob_store_path.clone();

        handle.confirm_sync().await.map_err(to_py_err)?;

        let response = handle
            .send_request(NotebookRequest::ExecuteCell {
                cell_id: cell_id.to_string(),
            })
            .await
            .map_err(to_py_err)?;

        match response {
            NotebookResponse::CellQueued { .. } => {}
            NotebookResponse::Error { error } => return Err(to_py_err(error)),
            other => return Err(to_py_err(format!("Unexpected response: {:?}", other))),
        }

        drop(st);

        collect_outputs(state, cell_id, blob_base_url, blob_store_path).await
    })
    .await;

    match result {
        Ok(Ok(exec_result)) => Ok(exec_result),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(to_py_err(format!(
            "Execution timed out after {} seconds",
            timeout_secs
        ))),
    }
}

/// Create a cell and execute it (convenience wrapper).
pub(crate) async fn run(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
    code: &str,
    timeout_secs: f64,
) -> PyResult<ExecutionResult> {
    let cell_id = create_cell(state, code, "code", None).await?;
    execute_cell(state, notebook_id, &cell_id, timeout_secs).await
}

/// Queue a cell for execution without waiting for the result.
pub(crate) async fn queue_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<()> {
    let st = state.lock().await;

    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.confirm_sync().await.map_err(to_py_err)?;

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
}

/// Wait for execution to complete, then read outputs from the Automerge doc.
///
/// Uses the broadcast stream only as a signal for when execution is done.
/// The Automerge document is the source of truth for cell outputs.
pub(crate) async fn collect_outputs(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
) -> PyResult<ExecutionResult> {
    let mut kernel_error: Option<String> = None;

    // Phase 1: Wait for ExecutionDone or KernelError signal via broadcast.
    loop {
        let mut st = state.lock().await;

        let broadcast_rx = st
            .broadcast_rx
            .as_mut()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let broadcast =
            tokio::time::timeout(std::time::Duration::from_millis(100), broadcast_rx.recv()).await;

        match broadcast {
            Ok(Some(msg)) => {
                drop(st);

                match msg {
                    NotebookBroadcast::ExecutionDone {
                        cell_id: msg_cell_id,
                    } => {
                        if msg_cell_id == cell_id {
                            log::debug!("[session_core] ExecutionDone received for {}", cell_id);
                            break;
                        }
                    }
                    NotebookBroadcast::KernelError { error } => {
                        log::debug!("[session_core] KernelError: {}", error);
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

    // KernelError: return immediately without touching the doc
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

        let cells = handle.get_cells();
        let snapshot = cells.into_iter().find(|c| c.id == cell_id).ok_or_else(|| {
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

/// Execute all code cells in order, returns the number of cells queued.
pub(crate) async fn run_all_cells(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
) -> PyResult<usize> {
    // Auto-start kernel
    {
        let st = state.lock().await;
        if !st.kernel_started {
            drop(st);
            ensure_kernel_started(state, notebook_id).await?;
        }
    }

    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.confirm_sync().await.map_err(to_py_err)?;

    let response = handle
        .send_request(NotebookRequest::RunAllCells {})
        .await
        .map_err(to_py_err)?;

    drop(st);

    match response {
        NotebookResponse::AllCellsQueued { count } => Ok(count),
        NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

// =========================================================================
// Presence
// =========================================================================

/// Set cursor position for presence.
pub(crate) async fn set_cursor(
    state: &Arc<Mutex<SessionState>>,
    peer_label: Option<&str>,
    cell_id: &str,
    line: u32,
    column: u32,
) -> PyResult<()> {
    let data = notebook_doc::presence::encode_cursor_update_labeled(
        "local",
        peer_label,
        &notebook_doc::presence::CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    );
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.send_presence(data).await.map_err(to_py_err)
}

/// Set selection range for presence.
pub(crate) async fn set_selection(
    state: &Arc<Mutex<SessionState>>,
    peer_label: Option<&str>,
    cell_id: &str,
    anchor_line: u32,
    anchor_col: u32,
    head_line: u32,
    head_col: u32,
) -> PyResult<()> {
    let data = notebook_doc::presence::encode_selection_update_labeled(
        "local",
        peer_label,
        &notebook_doc::presence::SelectionRange {
            cell_id: cell_id.to_string(),
            anchor_line,
            anchor_col,
            head_line,
            head_col,
        },
    );
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.send_presence(data).await.map_err(to_py_err)
}

// =========================================================================
// Notebook metadata
// =========================================================================

/// Set a notebook metadata key.
pub(crate) async fn set_metadata(
    state: &Arc<Mutex<SessionState>>,
    key: &str,
    value: &str,
) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.set_metadata(key, value).await.map_err(to_py_err)
}

/// Get a notebook metadata key.
pub(crate) async fn get_metadata(
    state: &Arc<Mutex<SessionState>>,
    key: &str,
) -> PyResult<Option<String>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.get_metadata(key).await.map_err(to_py_err)
}

/// Save the notebook to disk.
pub(crate) async fn save(state: &Arc<Mutex<SessionState>>, path: Option<&str>) -> PyResult<String> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let request = if let Some(p) = path {
        NotebookRequest::SaveNotebook {
            format_cells: false,
            path: Some(p.to_string()),
        }
    } else {
        NotebookRequest::SaveNotebook {
            format_cells: false,
            path: None,
        }
    };

    let response = handle.send_request(request).await.map_err(to_py_err)?;

    match response {
        NotebookResponse::NotebookSaved { path: saved_path } => Ok(saved_path),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

// =========================================================================
// Cell metadata
// =========================================================================

/// Get cell metadata as JSON string.
pub(crate) async fn get_cell_metadata(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<String>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let cells = handle.get_cells();
    let cell = cells.into_iter().find(|c| c.id == cell_id);

    match cell {
        Some(c) => Ok(Some(
            serde_json::to_string(&c.metadata)
                .map_err(|e| to_py_err(format!("Serialize: {}", e)))?,
        )),
        None => Ok(None),
    }
}

/// Set cell metadata from JSON string.
pub(crate) async fn set_cell_metadata(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    metadata_json: &str,
) -> PyResult<bool> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|e| to_py_err(format!("Invalid JSON: {}", e)))?;

    let result = handle
        .set_cell_metadata(cell_id, &metadata)
        .await
        .map_err(to_py_err)?;

    Ok(result)
}

/// Update cell metadata at a specific path.
pub(crate) async fn update_cell_metadata_at(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    path: Vec<String>,
    value_json: &str,
) -> PyResult<bool> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let value: serde_json::Value =
        serde_json::from_str(value_json).map_err(|e| to_py_err(format!("Invalid JSON: {}", e)))?;

    let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

    let result = handle
        .update_cell_metadata_at(cell_id, &path_refs, value)
        .await
        .map_err(to_py_err)?;

    Ok(result)
}

// =========================================================================
// Notebook-level metadata helpers (uv/conda dependencies)
// =========================================================================

/// Get the notebook metadata snapshot.
pub(crate) async fn get_notebook_metadata(
    state: &Arc<Mutex<SessionState>>,
) -> PyResult<NotebookMetadataSnapshot> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let snapshot = handle.get_notebook_metadata().unwrap_or_default();

    Ok(snapshot)
}

/// Set the notebook metadata snapshot.
pub(crate) async fn set_notebook_metadata(
    state: &Arc<Mutex<SessionState>>,
    snapshot: &NotebookMetadataSnapshot,
) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let json_str =
        serde_json::to_string(snapshot).map_err(|e| to_py_err(format!("Serialize: {}", e)))?;

    handle
        .set_metadata("notebook_metadata", &json_str)
        .await
        .map_err(to_py_err)
}

/// Sync environment with current metadata.
///
/// NOTE: This simplified version returns immediately on `SyncEnvironmentStarted`
/// without waiting for completion. The real implementations that poll the broadcast
/// stream for completion live inline in `session.rs` and `async_session.rs`.
#[allow(dead_code)]
pub(crate) async fn sync_environment(
    state: &Arc<Mutex<SessionState>>,
) -> PyResult<SyncEnvironmentResult> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::SyncEnvironment {})
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::SyncEnvironmentStarted { packages } => Ok(SyncEnvironmentResult {
            success: true,
            synced_packages: packages,
            error: None,
            needs_restart: false,
        }),
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
}

// =========================================================================
// Completion & History
// =========================================================================

/// Get code completions.
pub(crate) async fn complete(
    state: &Arc<Mutex<SessionState>>,
    code: &str,
    cursor_pos: usize,
) -> PyResult<CompletionResult> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::Complete {
            code: code.to_string(),
            cursor_pos,
        })
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::CompletionResult {
            items,
            cursor_start,
            cursor_end,
        } => Ok(CompletionResult {
            items: items
                .into_iter()
                .map(CompletionItem::from_protocol)
                .collect(),
            cursor_start,
            cursor_end,
        }),
        NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Get execution history.
pub(crate) async fn get_history(
    state: &Arc<Mutex<SessionState>>,
    pattern: Option<&str>,
    n: i32,
    unique: bool,
) -> PyResult<Vec<HistoryEntry>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::GetHistory {
            pattern: pattern.map(|s| s.to_string()),
            n: n as i32,
            unique,
        })
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::HistoryResult { entries } => Ok(entries
            .into_iter()
            .map(HistoryEntry::from_protocol)
            .collect()),
        NotebookResponse::NoKernel {} => Err(to_py_err("No kernel running")),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Get the execution queue state.
pub(crate) async fn get_queue_state(state: &Arc<Mutex<SessionState>>) -> PyResult<QueueState> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    let response = handle
        .send_request(NotebookRequest::GetQueueState {})
        .await
        .map_err(to_py_err)?;

    match response {
        NotebookResponse::QueueState { executing, queued } => Ok(QueueState { executing, queued }),
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Ensure a kernel is started, connecting first if needed.
async fn ensure_kernel_started(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
) -> PyResult<()> {
    // Connect if needed
    {
        let st = state.lock().await;
        if st.handle.is_none() {
            drop(st);
            connect(state, notebook_id).await?;
        }
    }

    start_kernel(state, "python", "auto", None).await
}

/// Resolve blob server URL and store path from daemon info.
async fn resolve_blob_paths(socket_path: &PathBuf) -> (Option<String>, Option<PathBuf>) {
    if let Some(parent) = socket_path.parent() {
        let daemon_json = parent.join("daemon.json");
        let base_url = if daemon_json.exists() {
            tokio::fs::read_to_string(&daemon_json)
                .await
                .ok()
                .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
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
    }
}
