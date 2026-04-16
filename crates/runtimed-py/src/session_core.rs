//! Shared async core for Session and AsyncSession.
//!
//! All business logic lives here as free async functions operating on
//! `Arc<Mutex<SessionState>>`. The sync `Session` calls these via
//! `runtime.block_on()`, and `AsyncSession` calls them via
//! `future_into_py()`. This eliminates the duplication that previously
//! existed between session.rs and async_session.rs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use notebook_protocol::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};
use notebook_sync::{BroadcastReceiver, DocHandle};

use notebook_doc::metadata::NotebookMetadataSnapshot;

use crate::daemon_paths::get_socket_path;
use crate::error::to_py_err;
use crate::output::{
    Cell, CompletionItem, CompletionResult, ExecutionResult, HistoryEntry, NotebookConnectionInfo,
    Output, PyQueueEntry, PyRuntimeState, QueueState, SyncEnvironmentResult,
};
use crate::output_resolver;

use pyo3::prelude::*;

/// Default actor label for Python binding sessions when no peer_label is provided.
const DEFAULT_ACTOR_LABEL: &str = "runtimed-py";

/// Remote cursor info: (peer_id, peer_label, cell_id, line, column).
pub(crate) type RemoteCursor = (String, String, String, u32, u32);

// =========================================================================
// Shared state
// =========================================================================

/// Internal state shared between Session and AsyncSession.
///
/// Both wrappers hold `Arc<Mutex<SessionState>>` and delegate all
/// async operations to the free functions in this module.
pub(crate) struct SessionState {
    /// DocHandle for direct document access and daemon protocol operations.
    pub handle: Option<DocHandle>,
    /// Broadcast receiver for kernel/execution events from the daemon.
    pub broadcast_rx: Option<BroadcastReceiver>,
    pub kernel_started: bool,
    pub kernel_type: Option<String>,
    pub env_source: Option<String>,
    /// Intended runtime type for this session (e.g. "python", "deno").
    /// Set at creation/open time; used by ensure_kernel_started to avoid
    /// hardcoding "python" when auto-launching kernels.
    pub runtime: String,
    /// Base URL for blob server (for resolving blob hashes)
    pub blob_base_url: Option<String>,
    /// Path to blob store directory (fallback for direct disk access)
    pub blob_store_path: Option<PathBuf>,
    /// Connection info from daemon (for open_notebook/create_notebook)
    pub connection_info: Option<NotebookConnectionInfo>,
    /// Notebook path (for project file detection during kernel launch)
    pub notebook_path: Option<String>,
    /// User settings (synced from daemon at connection time)
    pub settings: Option<runtimed::settings_doc::SyncedSettings>,
    /// Peer label for presence (e.g., "Claude", "Agent")
    pub peer_label: Option<String>,
    /// Actor label for Automerge provenance (e.g., "agent:claude:ab12cd34")
    pub actor_label: Option<String>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            handle: None,
            broadcast_rx: None,
            kernel_started: false,
            kernel_type: None,
            env_source: None,
            runtime: "python".to_string(),
            blob_base_url: None,
            blob_store_path: None,
            connection_info: None,
            notebook_path: None,
            settings: None,
            peer_label: None,
            actor_label: None,
        }
    }
}

/// Generate a default peer label when none is provided.
///
/// Produces `"peer-<short_random>"`, e.g. `"peer-ab12cd34"`, so each
/// unnamed session is still distinguishable in the peers list.
pub(crate) fn default_peer_label() -> String {
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    format!("peer-{}", short_id)
}

/// Build an actor label from a peer display name.
///
/// The format is `"agent:<lowercased_name>:<short_random>"`, e.g.
/// `"agent:claude:ab12cd34"`. The random suffix makes each session
/// unique even when the same agent reconnects.
pub(crate) fn make_actor_label(peer_label: &str) -> String {
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    format!("agent:{}:{}", peer_label.to_lowercase(), short_id)
}

// =========================================================================
// Settings
// =========================================================================

/// Sync settings from daemon and return the parsed settings.
///
/// This performs a one-shot Automerge sync with the daemon's settings document.
/// Returns None if the connection fails (graceful degradation).
pub(crate) async fn sync_settings(
    socket_path: PathBuf,
) -> Option<runtimed::settings_doc::SyncedSettings> {
    match runtimed::sync_client::SyncClient::connect(socket_path).await {
        Ok(client) => Some(client.get_all()),
        Err(e) => {
            log::warn!("[session-core] Settings sync failed: {}", e);
            None
        }
    }
}

/// Get settings from session state.
pub(crate) fn get_settings(state: &SessionState) -> Option<runtimed::settings_doc::SyncedSettings> {
    state.settings.clone()
}

/// Get the notebook's environment type from metadata structure.
///
/// Returns "conda" if conda metadata exists, "uv" if uv metadata exists, None otherwise.
/// This checks if the metadata structure exists, not whether deps are non-empty.
pub(crate) fn get_metadata_env_type(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    if snapshot.runt.conda.is_some() {
        return Some("conda".to_string());
    }
    if snapshot.runt.uv.is_some() {
        return Some("uv".to_string());
    }
    None
}

// =========================================================================
// Connection
// =========================================================================

/// Connect to the daemon if not already connected.
///
/// Populates the state with handle, broadcast_rx, and blob paths.
pub(crate) async fn connect(state: &Arc<Mutex<SessionState>>, notebook_id: &str) -> PyResult<()> {
    let socket_path = get_socket_path();
    connect_with_socket(state, notebook_id, socket_path).await
}

/// Connect to the daemon using a specific socket path.
///
/// Populates the state with handle, broadcast_rx, and blob paths.
pub(crate) async fn connect_with_socket(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
    socket_path: PathBuf,
) -> PyResult<()> {
    let mut st = state.lock().await;
    if st.handle.is_some() {
        return Ok(());
    }

    let actor_label = st
        .actor_label
        .clone()
        .unwrap_or_else(|| make_actor_label(DEFAULT_ACTOR_LABEL));

    let result =
        notebook_sync::connect::connect(socket_path.clone(), notebook_id.to_string(), &actor_label)
            .await
            .map_err(to_py_err)?;

    // Resolve blob paths from daemon info
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    st.handle = Some(result.handle);
    st.broadcast_rx = Some(result.broadcast_rx);
    st.blob_base_url = blob_base_url;
    st.blob_store_path = blob_store_path;

    // Infer runtime from the notebook's kernelspec when joining an existing notebook
    if let Some(meta) = st.handle.as_ref().and_then(|h| h.get_notebook_metadata()) {
        if let Some(ref ks) = meta.kernelspec {
            if ks.name == "deno" {
                st.runtime = "deno".to_string();
            }
        }
    }

    hydrate_kernel_state(&mut st);

    Ok(())
}

/// Send an initial presence message so the daemon registers this peer immediately.
///
/// Without this, the peer is invisible in `.peers` until it performs an action
/// that emits presence (e.g. editing a cell). Call after `peer_label` is set.
pub(crate) async fn announce_presence(state: &SessionState) {
    let handle = match state.handle.as_ref() {
        Some(h) => h,
        None => return,
    };
    let peer_label = state.peer_label.as_deref();
    let actor_label = state.actor_label.as_deref();
    let first_cell_id = handle.first_cell_id();

    let data = if let Some(cell_id) = first_cell_id {
        notebook_doc::presence::encode_focus_update_labeled(
            "local",
            peer_label,
            actor_label,
            &cell_id,
        )
    } else {
        notebook_doc::presence::encode_custom_update_labeled("local", peer_label, actor_label, &[])
    };
    let _ = handle.send_presence(data).await;
}

/// Populate `kernel_started`, `kernel_type`, and `env_source` from the
/// RuntimeStateDoc (the daemon's source of truth for kernel status).
///
/// Best-effort: silently does nothing if the handle is missing or the
/// runtime state can't be read (e.g. brand-new notebook with no state yet).
fn hydrate_kernel_state(state: &mut SessionState) {
    let Some(handle) = state.handle.as_ref() else {
        return;
    };
    let Ok(rs) = handle.get_runtime_state() else {
        return;
    };
    let running = matches!(rs.kernel.status.as_str(), "idle" | "busy" | "starting");
    if running {
        state.kernel_started = true;
        state.kernel_type = Some(rs.kernel.language.clone());
        state.env_source = Some(rs.kernel.env_source.clone());
    }
}

/// Spawn a background task that listens for `RoomRenamed` broadcasts
/// and updates the `notebook_id_override` when another peer re-keys the room.
///
/// This ensures `Session.notebook_id` / `AsyncSession.notebook_id` stays
/// correct even when a different peer saves an ephemeral notebook.
pub(crate) fn spawn_rekey_watcher(
    broadcast_rx: &BroadcastReceiver,
    notebook_id_override: Arc<std::sync::Mutex<Option<String>>>,
    handle: &tokio::runtime::Handle,
) {
    let mut rx = broadcast_rx.resubscribe();
    handle.spawn(async move {
        loop {
            match rx.recv().await {
                Some(NotebookBroadcast::RoomRenamed { new_notebook_id }) => {
                    log::info!(
                        "[session] Room re-keyed by peer, new notebook_id: {}",
                        new_notebook_id
                    );
                    *notebook_id_override.lock().unwrap() = Some(new_notebook_id);
                }
                Some(_) => continue,
                None => break, // channel closed
            }
        }
    });
}

/// Connect and open an existing notebook file.
///
/// Returns (notebook_id, populated SessionState, NotebookConnectionInfo).
pub(crate) async fn connect_open(
    socket_path: PathBuf,
    path: &str,
    actor_label: Option<&str>,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
    let default_label;
    let label = match actor_label {
        Some(l) => l,
        None => {
            default_label = make_actor_label(DEFAULT_ACTOR_LABEL);
            &default_label
        }
    };
    let result =
        notebook_sync::connect::connect_open(socket_path.clone(), PathBuf::from(path), label)
            .await
            .map_err(to_py_err)?;

    let notebook_id = result.info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;
    let connection_info = NotebookConnectionInfo::from_protocol(result.info);

    // Sync settings from daemon (best-effort, don't fail if unavailable)
    let settings = sync_settings(socket_path).await;

    // Infer runtime from the notebook's kernelspec (if present)
    let runtime = result
        .handle
        .get_notebook_metadata()
        .and_then(|meta| meta.kernelspec)
        .map(|ks| {
            if ks.name == "deno" {
                "deno".to_string()
            } else {
                "python".to_string()
            }
        })
        .unwrap_or_else(|| "python".to_string());

    let mut state = SessionState {
        handle: Some(result.handle),
        broadcast_rx: Some(result.broadcast_rx),
        kernel_started: false,
        kernel_type: None,
        env_source: None,
        runtime,
        blob_base_url,
        blob_store_path,
        connection_info: Some(connection_info.clone()),
        notebook_path: Some(path.to_string()),
        settings,
        peer_label: None, // Set by caller (Session/AsyncSession)
        actor_label: actor_label.map(String::from),
    };

    hydrate_kernel_state(&mut state);

    Ok((notebook_id, state, connection_info))
}

/// Connect and create a new notebook.
///
/// Returns (notebook_id, populated SessionState, NotebookConnectionInfo).
pub(crate) async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: Option<&str>,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
    let default_label;
    let label = match actor_label {
        Some(l) => l,
        None => {
            default_label = make_actor_label(DEFAULT_ACTOR_LABEL);
            &default_label
        }
    };
    let result = notebook_sync::connect::connect_create(
        socket_path.clone(),
        runtime,
        working_dir.clone(),
        label,
        false,
    )
    .await
    .map_err(to_py_err)?;

    let notebook_id = result.info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;
    let connection_info = NotebookConnectionInfo::from_protocol(result.info);

    // Sync settings from daemon (best-effort, don't fail if unavailable)
    let settings = sync_settings(socket_path).await;

    let mut state = SessionState {
        handle: Some(result.handle),
        broadcast_rx: Some(result.broadcast_rx),
        kernel_started: false,
        kernel_type: None,
        env_source: None,
        runtime: runtime.to_string(),
        blob_base_url,
        blob_store_path,
        connection_info: Some(connection_info.clone()),
        notebook_path: working_dir.map(|p| p.to_string_lossy().to_string()),
        settings,
        peer_label: None, // Set by caller (Session/AsyncSession)
        actor_label: actor_label.map(String::from),
    };

    hydrate_kernel_state(&mut state);

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
            kernel_type: actual_type,
            env_source: actual_env,
            ..
        } => {
            st.kernel_started = true;
            st.kernel_type = Some(actual_type.clone());
            st.env_source = Some(actual_env);
            if kernel_type != actual_type {
                return Err(to_py_err(format!(
                    "Kernel type mismatch: requested '{}' but '{}' is already running",
                    kernel_type, actual_type
                )));
            }
            Ok(())
        }
        NotebookResponse::KernelAlreadyRunning {
            kernel_type: actual_type,
            env_source: actual_env,
            ..
        } => {
            st.kernel_started = true;
            st.kernel_type = Some(actual_type.clone());
            st.env_source = Some(actual_env);
            if kernel_type != actual_type {
                return Err(to_py_err(format!(
                    "Kernel type mismatch: requested '{}' but '{}' is already running",
                    kernel_type, actual_type
                )));
            }
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
            st.kernel_type = None;
            st.env_source = None;
            Ok(())
        }
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Restart the kernel with auto environment detection.
///
/// Returns a list of progress messages emitted during environment
/// preparation (e.g. "Installing 3 packages..."). Empty if cached.
pub(crate) async fn restart_kernel(
    state: &Arc<Mutex<SessionState>>,
    wait_for_ready: bool,
) -> PyResult<Vec<String>> {
    // Capture the current kernel_type and env_source before shutdown clears them,
    // so we re-launch with the same configuration.
    let (prev_kernel_type, prev_env_source) = {
        let st = state.lock().await;
        (st.kernel_type.clone(), st.env_source.clone())
    };

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
                st.kernel_type = None;
                st.env_source = None;
            }
            NotebookResponse::Error { error } => return Err(to_py_err(error)),
            _ => {}
        }
    }

    // Clone handle and resubscribe broadcast_rx so we can release the lock
    // before the potentially long-running LaunchKernel request.
    let (handle, resolved_path, mut progress_rx) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone();
        let resolved_path = st.notebook_path.clone();
        let progress_rx = st.broadcast_rx.as_ref().map(|rx| rx.resubscribe());
        (handle, resolved_path, progress_rx)
    };
    // Lock is now released — other operations can proceed.

    // Re-launch with the same kernel_type and env_source as before,
    // falling back to "python" / "auto" if no kernel was previously running.
    // Prewarmed envs use scoped auto-detect on restart so the daemon re-checks
    // metadata and picks up any deps added after launch (e.g. via add_dependency),
    // while staying within the original package manager family.
    let restart_kernel_type = prev_kernel_type.unwrap_or_else(|| "python".to_string());
    let restart_env_source = match prev_env_source.as_deref() {
        Some("uv:prewarmed") => "auto:uv".to_string(),
        Some("conda:prewarmed") => "auto:conda".to_string(),
        None => "auto".to_string(),
        Some(s) => s.to_string(),
    };

    // Send LaunchKernel with a timeout, collecting progress messages concurrently.
    let mut progress_messages: Vec<String> = Vec::new();
    let launch_timeout = std::time::Duration::from_secs(120);

    let launch_fut = handle.send_request(NotebookRequest::LaunchKernel {
        kernel_type: restart_kernel_type,
        env_source: restart_env_source,
        notebook_path: resolved_path,
    });

    let response = if let Some(ref mut prx) = progress_rx {
        // Race between launch response and progress events
        tokio::pin!(launch_fut);
        let deadline = tokio::time::Instant::now() + launch_timeout;
        loop {
            tokio::select! {
                resp = &mut launch_fut => {
                    break resp.map_err(to_py_err)?;
                }
                msg = prx.recv() => {
                    if let Some(NotebookBroadcast::EnvProgress { env_type, phase }) = msg {
                        let text = env_progress_message(&phase);
                        progress_messages.push(format!("[{}] {}", env_type, text));
                    }
                    // Continue waiting for launch response
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(to_py_err(
                        "Kernel restart timed out after 120s (environment may still be installing)"
                    ));
                }
            }
        }
    } else {
        tokio::time::timeout(launch_timeout, launch_fut)
            .await
            .map_err(|_| {
                to_py_err(
                    "Kernel restart timed out after 120s (environment may still be installing)",
                )
            })?
            .map_err(to_py_err)?
    };

    // Re-acquire lock to update state
    {
        let mut st = state.lock().await;
        match response {
            NotebookResponse::KernelLaunched {
                kernel_type: actual_type,
                env_source: actual_env,
                ..
            }
            | NotebookResponse::KernelAlreadyRunning {
                kernel_type: actual_type,
                env_source: actual_env,
                ..
            } => {
                st.kernel_started = true;
                st.kernel_type = Some(actual_type);
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
                        return Ok(progress_messages);
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => return Err(to_py_err("Broadcast channel closed")),
                    Err(_) => continue,
                }
            }
        }
    }

    Ok(progress_messages)
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

/// Compute the (line, column) position at the end of a source string.
///
/// Unlike `str::lines()` which drops a trailing empty line, this correctly
/// returns (N, 0) when the source ends with '\n'.
fn end_of_source_position(source: &str) -> (u32, u32) {
    let line = source.as_bytes().iter().filter(|&&b| b == b'\n').count() as u32;
    if source.is_empty() || source.ends_with('\n') {
        (line, 0)
    } else {
        let col = source.rsplit('\n').next().unwrap_or(source).len() as u32;
        (line, col)
    }
}

/// Create a new cell with source (atomic operation).
pub(crate) async fn create_cell(
    state: &Arc<Mutex<SessionState>>,
    source: &str,
    cell_type: &str,
    index: Option<usize>,
) -> PyResult<String> {
    let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Determine after_cell_id from index.
        // None → append at end; Some(0) → prepend; Some(i) → after cell i-1.
        // Out-of-range indices are clamped to append.
        let after_cell_id = match index {
            Some(0) => None,
            None => handle.last_cell_id(),
            Some(i) => {
                let cell_ids = handle.get_cell_ids();
                let clamped = i.min(cell_ids.len());
                cell_ids.get(clamped.saturating_sub(1)).cloned()
            }
        };

        handle
            .add_cell_with_source(&cell_id, cell_type, after_cell_id.as_deref(), source)
            .map_err(to_py_err)?;
    }

    // Emit presence at end of new source
    let (last_line, last_col) = end_of_source_position(source);
    emit_cursor_presence(state, &cell_id, last_line, last_col).await;

    Ok(cell_id)
}

/// Update a cell's source.
pub(crate) async fn set_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    source: &str,
) -> PyResult<()> {
    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle.update_source(cell_id, source).map_err(to_py_err)?;
    }

    // Emit presence at end of new source (single pass, no allocation)
    let (last_line, last_col) = end_of_source_position(source);
    emit_cursor_presence(state, cell_id, last_line, last_col).await;
    emit_clear_channel(state, notebook_doc::presence::Channel::Selection).await;

    Ok(())
}

/// Splice a cell's source at a specific position (character-level, no diff).
/// Deletes `delete_count` characters starting at `index`, then inserts `text`.
pub(crate) async fn splice_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    index: usize,
    delete_count: usize,
    text: &str,
) -> PyResult<()> {
    let (last_line, last_col) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle
            .splice_source(cell_id, index, delete_count, text)
            .map_err(to_py_err)?;

        // Read back the full source to compute cursor position at splice point
        let cell = handle
            .get_cell(cell_id)
            .ok_or_else(|| to_py_err(format!("Cell {} not found", cell_id)))?;

        // Position the cursor at the end of the inserted text.
        // Use char count (not byte length) — Automerge indices are character-based.
        let cursor_index = index + text.chars().count();
        index_to_line_col(&cell.source, cursor_index)
    };

    emit_cursor_presence(state, cell_id, last_line, last_col).await;
    emit_clear_channel(state, notebook_doc::presence::Channel::Selection).await;

    Ok(())
}

/// Convert a character index in a string to (line, col) — both 0-based, u32 for presence API.
/// Uses character counting (not byte offsets) to handle multi-byte UTF-8 correctly.
fn index_to_line_col(source: &str, char_index: usize) -> (u32, u32) {
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    for (i, ch) in source.chars().enumerate() {
        if i >= char_index {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Append text to a cell's source.
pub(crate) async fn append_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    text: &str,
) -> PyResult<()> {
    // Compute cursor position at end of the full source after append.
    // We need the current source + appended text to find the last line/col.
    let (last_line, last_col) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle.append_source(cell_id, text).map_err(to_py_err)?;

        // Read back the full source to compute end position
        let cell = handle
            .get_cell(cell_id)
            .ok_or_else(|| to_py_err(format!("Cell {} not found", cell_id)))?;

        end_of_source_position(&cell.source)
    };

    emit_cursor_presence(state, cell_id, last_line, last_col).await;
    emit_clear_channel(state, notebook_doc::presence::Channel::Selection).await;

    Ok(())
}

/// Set a cell's type.
pub(crate) async fn set_cell_type(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    cell_type: &str,
) -> PyResult<()> {
    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle
            .set_cell_type(cell_id, cell_type)
            .map_err(to_py_err)?;
    }

    // Emit focus presence — cell-level operation, no cursor position
    emit_focus_presence(state, cell_id).await;

    Ok(())
}

/// Get a single cell by ID, with resolved outputs.
pub(crate) async fn get_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<Cell> {
    let (snapshot, blob_base_url, blob_store_path, comms) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let blob_base_url = st.blob_base_url.clone();
        let blob_store_path = st.blob_store_path.clone();
        let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);

        let snapshot = handle
            .get_cell(cell_id)
            .ok_or_else(|| to_py_err(format!("Cell not found: {}", cell_id)))?;

        (snapshot, blob_base_url, blob_store_path, comms)
    };

    let outputs = output_resolver::resolve_cell_outputs(
        &snapshot.outputs,
        &blob_base_url,
        &blob_store_path,
        comms.as_ref(),
    )
    .await;

    Ok(Cell::from_snapshot_with_outputs(snapshot, outputs))
}

/// Get all cells with resolved outputs.
pub(crate) async fn get_cells(state: &Arc<Mutex<SessionState>>) -> PyResult<Vec<Cell>> {
    let (snapshots, blob_base_url, blob_store_path, comms) = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        let blob_base_url = st.blob_base_url.clone();
        let blob_store_path = st.blob_store_path.clone();
        let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
        let snapshots = handle.get_cells();

        (snapshots, blob_base_url, blob_store_path, comms)
    };

    let mut cells = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let outputs = output_resolver::resolve_cell_outputs(
            &snapshot.outputs,
            &blob_base_url,
            &blob_store_path,
            comms.as_ref(),
        )
        .await;
        cells.push(Cell::from_snapshot_with_outputs(snapshot, outputs));
    }

    Ok(cells)
}

/// Get a cell's source text without materializing all cells.
pub(crate) async fn get_cell_source(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<String>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_source(cell_id))
}

/// Get a cell's type (e.g. "code", "markdown") without materializing all cells.
pub(crate) async fn get_cell_type(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<String>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_type(cell_id))
}

/// Get a cell's raw output manifests as JSON strings without blob resolution.
pub(crate) async fn get_cell_outputs(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<Vec<String>>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_outputs(cell_id).map(|values| {
        values
            .into_iter()
            .map(|v| serde_json::to_string(&v).unwrap_or_default())
            .collect()
    }))
}

/// Get a cell's execution count without materializing all cells.
pub(crate) async fn get_cell_execution_count(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<String>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_execution_count(cell_id))
}

/// Get all cell IDs in document order without materializing full cell data.
pub(crate) async fn get_cell_ids(state: &Arc<Mutex<SessionState>>) -> PyResult<Vec<String>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_ids())
}

/// Get a cell's fractional-index position string without materializing all cells.
pub(crate) async fn get_cell_position(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> PyResult<Option<String>> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };
    Ok(handle.get_cell_position(cell_id))
}

/// Delete a cell.
pub(crate) async fn delete_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<()> {
    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle.delete_cell(cell_id).map_err(to_py_err)?;
    }

    // Cell is gone — clear any stale cursor and selection
    emit_clear_channel(state, notebook_doc::presence::Channel::Cursor).await;
    emit_clear_channel(state, notebook_doc::presence::Channel::Selection).await;

    Ok(())
}

/// Move a cell to a new position.
pub(crate) async fn move_cell(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    after_cell_id: Option<&str>,
) -> PyResult<String> {
    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        // Synchronous — direct doc mutation via DocHandle
        handle
            .move_cell(cell_id, after_cell_id)
            .map_err(to_py_err)?;
    }

    // Emit focus presence — cell-level operation, no cursor position
    emit_focus_presence(state, cell_id).await;

    Ok(cell_id.to_string())
}

/// Clear a cell's outputs.
///
/// Sends a ClearOutputs request to the daemon — outputs live in RuntimeStateDoc,
/// so the daemon handles clearing the execution_id pointer and outputs.
pub(crate) async fn clear_outputs(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> PyResult<()> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };

    handle
        .send_request(NotebookRequest::ClearOutputs {
            cell_id: cell_id.to_string(),
        })
        .await
        .map_err(|e| to_py_err(format!("Failed to clear outputs: {e}")))?;

    // Emit focus presence — cell-level operation on outputs, not source
    emit_focus_presence(state, cell_id).await;
    Ok(())
}

// =========================================================================
// Execution
// =========================================================================

/// Execute a cell and return the result.
///
/// Wait for an already-queued execution to complete and return its outputs.
///
/// Unlike `execute_cell()`, this does NOT re-queue the cell. It assumes
/// the caller already has an `execution_id` from a prior `queue_cell()`.
/// This is the correct path for `Execution.result()` — it waits for the
/// specific execution to finish without risking a duplicate execution.
pub(crate) async fn wait_for_execution(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    execution_id: &str,
    timeout_secs: f64,
) -> PyResult<ExecutionResult> {
    let timeout = std::time::Duration::from_secs_f64(timeout_secs);
    let result = tokio::time::timeout(timeout, async {
        let (blob_base_url, blob_store_path) = {
            let st = state.lock().await;
            (st.blob_base_url.clone(), st.blob_store_path.clone())
        };

        collect_outputs(
            state,
            cell_id,
            Some(execution_id),
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
            "Execution timed out after {} seconds (execution_id={})",
            timeout_secs, execution_id
        ))),
    }
}

///
/// The entire lifecycle (confirm_sync, send_request, collect_outputs)
/// is wrapped in a single timeout.
///
/// Internally uses `queue_cell()` as the single primitive for submitting
/// execution requests, then waits for outputs via `collect_outputs()`.
pub(crate) async fn execute_cell(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
    cell_id: &str,
    timeout_secs: f64,
) -> PyResult<ExecutionResult> {
    let timeout = std::time::Duration::from_secs_f64(timeout_secs);
    let result = tokio::time::timeout(timeout, async {
        // queue_cell is the single primitive: auto-starts kernel, confirms
        // sync, sends ExecuteCell request, emits focus presence.
        let execution_id = queue_cell(state, notebook_id, cell_id).await?;

        let (blob_base_url, blob_store_path) = {
            let st = state.lock().await;
            (st.blob_base_url.clone(), st.blob_store_path.clone())
        };

        collect_outputs(
            state,
            cell_id,
            Some(&execution_id),
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
            timeout_secs
        ))),
    }
}

/// Queue a cell for execution without waiting for the result.
pub(crate) async fn queue_cell(
    state: &Arc<Mutex<SessionState>>,
    notebook_id: &str,
    cell_id: &str,
) -> PyResult<String> {
    // Auto-start kernel if not running (matches execute_cell behavior)
    {
        let st = state.lock().await;
        if !st.kernel_started {
            drop(st);
            ensure_kernel_started(state, notebook_id).await?;
        }
    }

    let response = {
        let st = state.lock().await;

        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle.confirm_sync().await.map_err(to_py_err)?;

        handle
            .send_request(NotebookRequest::ExecuteCell {
                cell_id: cell_id.to_string(),
            })
            .await
            .map_err(to_py_err)?
    };

    match response {
        NotebookResponse::CellQueued { execution_id, .. } => {
            // Don't emit focus here — it would overwrite any cursor presence
            // set by a prior edit (e.g. splice_source, set_source). The cursor
            // from the edit should persist through execution.
            Ok(execution_id)
        }
        NotebookResponse::Error { error } => Err(to_py_err(error)),
        other => Err(to_py_err(format!("Unexpected response: {:?}", other))),
    }
}

/// Wait for execution to complete, then read outputs from the Automerge doc.
///
/// When an `execution_id` is provided, defers to
/// [`notebook_sync::await_execution_terminal`] and reads outputs directly
/// from the `RuntimeStateDoc` — the same path `runt-mcp` uses. This avoids
/// the stream-output race where the `ExecutionDone` broadcast arrives
/// before the final stream manifests have synced into our replica.
///
/// When `execution_id` is `None` (legacy callers), falls back to the older
/// queue-presence polling + `NotebookDoc` cell snapshot read.
pub(crate) async fn collect_outputs(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    execution_id: Option<&str>,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
) -> PyResult<ExecutionResult> {
    // ── Fast path: execution_id known — use the shared helper ─────────
    if let Some(eid) = execution_id {
        let handle = {
            let st = state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| to_py_err("Not connected"))?
                .clone()
        };

        // No outer timeout here: the caller's tokio::time::timeout at
        // execute_cell()/wait_for_execution() bounds the total wait. Pass
        // a very long timeout so the helper only returns on terminal state
        // or kernel failure.
        let helper_timeout = std::time::Duration::from_secs(60 * 60 * 24);
        let terminal =
            notebook_sync::await_execution_terminal(&handle, eid, helper_timeout, None).await;

        match terminal {
            Ok(state) => {
                let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
                let outputs = output_resolver::resolve_cell_outputs(
                    &state.output_manifests,
                    &blob_base_url,
                    &blob_store_path,
                    comms.as_ref(),
                )
                .await;
                let success = state.status == "done"
                    && state.success
                    && !outputs.iter().any(|o| o.output_type == "error");
                return Ok(ExecutionResult {
                    cell_id: cell_id.to_string(),
                    outputs,
                    success,
                    execution_count: state.execution_count,
                });
            }
            Err(notebook_sync::ExecutionTerminalError::KernelFailed { reason }) => {
                return Ok(ExecutionResult {
                    cell_id: cell_id.to_string(),
                    outputs: vec![Output::error("KernelError", &reason, vec![])],
                    success: false,
                    execution_count: None,
                });
            }
            Err(notebook_sync::ExecutionTerminalError::Timeout) => {
                // Helper timeout is a day; reaching this means something
                // drove us there intentionally (cancellation, outer abort).
                return Err(to_py_err("Execution wait aborted"));
            }
        }
    }

    // ── Legacy path: no execution_id ─────────────────────────────────
    // Falls through to queue-presence polling + NotebookDoc cell read.
    let mut kernel_error: Option<String> = None;

    // Phase 1: Wait for the cell to leave the execution queue.
    //
    // Poll the RuntimeStateDoc queue — when the cell is no longer in
    // `executing` or `queued`, execution is done. Also check for kernel
    // error/shutdown status.
    //
    // IMPORTANT: The RuntimeStateDoc replica may not have synced yet when
    // we first poll (the daemon writes queue state, then Automerge sync
    // propagates it). We must see the cell IN the queue at least once
    // before treating its absence as completion. Otherwise we'd return
    // immediately with empty outputs.
    let mut seen_in_queue = false;

    loop {
        // Check RuntimeStateDoc queue state
        let cell_done_via_doc = {
            let st = state.lock().await;
            if let Some(handle) = st.handle.as_ref() {
                if let Ok(rs) = handle.get_runtime_state() {
                    // Fast path: if we have an execution_id and the executions
                    // map shows it's already done/error, we're finished. This
                    // handles the late-consumer case where Execution.result()
                    // is called after the execution has already completed.
                    let mut done = false;
                    if let Some(eid) = execution_id {
                        if let Some(exec_state) = rs.executions.get(eid) {
                            if exec_state.status == "done" || exec_state.status == "error" {
                                log::debug!(
                                    "[session_core] Late consumer: execution {} already {}",
                                    eid,
                                    exec_state.status
                                );
                                done = true;
                            } else {
                                seen_in_queue = true;
                            }
                        }
                        // else: execution entry not synced yet — fall through
                    }

                    // Fallback: kernel error or shutdown → stop waiting.
                    // These must run even when we have an execution_id,
                    // otherwise a kernel crash leaves us spinning until
                    // the outer timeout fires.
                    if !done {
                        if rs.kernel.status == "error" {
                            kernel_error = Some("Kernel error".to_string());
                            done = true;
                        } else if rs.kernel.status == "shutdown" {
                            kernel_error = Some("Kernel shut down".to_string());
                            done = true;
                        }
                    }

                    // Fallback: queue-presence tracking. If the cell was
                    // seen in the queue and has since left, execution is
                    // done regardless of whether the executions map or
                    // broadcasts caught up.
                    if !done {
                        let in_executing = rs.queue.executing.as_ref().is_some_and(|e| {
                            e.cell_id == cell_id
                                && execution_id.is_none_or(|eid| e.execution_id == eid)
                        });
                        let in_queued = rs.queue.queued.iter().any(|e| {
                            e.cell_id == cell_id
                                && execution_id.is_none_or(|eid| e.execution_id == eid)
                        });
                        let in_queue = in_executing || in_queued;

                        if in_queue {
                            seen_in_queue = true;
                        } else if seen_in_queue {
                            // Was in queue, now gone → done
                            done = true;
                        }
                        // else: never seen in queue — doc hasn't synced yet
                    }

                    done
                } else {
                    false
                }
            } else {
                false
            }
        };

        if cell_done_via_doc {
            log::debug!(
                "[session_core] Cell {} left queue (RuntimeStateDoc)",
                cell_id
            );
            break;
        }

        // Also drain broadcasts for KernelError as a fallback signal
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
                        cell_id: msg_cell_id,
                        execution_id: msg_exec_id,
                    })) => {
                        if msg_cell_id == cell_id
                            && execution_id.is_none_or(|eid| eid == msg_exec_id)
                        {
                            log::debug!("[session_core] ExecutionDone broadcast for {}", cell_id);
                            break;
                        }
                    }
                    Ok(Some(NotebookBroadcast::KernelError { error })) => {
                        log::debug!("[session_core] KernelError: {}", error);
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
    //
    // The cell has left the execution queue, but the daemon's output writes
    // may not have synced back to our local replica yet.  confirm_sync only
    // ensures the daemon has *our* changes — not that we have *theirs*.
    //
    // Poll with confirm_sync until outputs appear (or a timeout).  Every
    // execution produces at least one output or an execution_count change,
    // so an empty snapshot after queue departure means sync is still lagging.
    //
    // TODO(#1048): Replace this sleep-and-retry with the execution handle
    // abstraction.  Once execution lifecycle (status, outputs) lives in the
    // RuntimeStateDoc keyed by execution_id, we can wait on the authoritative
    // "done"/"error" status instead of polling the notebook doc for outputs.
    //
    // NOTE(#1066): The cell snapshot below is read by cell_id, not execution_id.
    // In a multi-client scenario, another peer re-executing the same cell can
    // overwrite the doc snapshot between our ExecutionDone and this read,
    // returning outputs from the wrong execution. This is fixed once cells
    // carry an execution_id pointer and outputs are keyed by execution_id
    // in the RuntimeStateDoc.
    let mut snapshot = None;
    let mut blob_base_url_out = None;
    let mut blob_store_path_out = None;
    let mut comms_out = None;

    for attempt in 0..5 {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle.confirm_sync().await.map_err(to_py_err)?;
        handle.confirm_state_sync().await.map_err(to_py_err)?;

        let snap = handle.get_cell(cell_id).ok_or_else(|| {
            to_py_err(format!(
                "Cell not found in doc after execution: {}",
                cell_id
            ))
        })?;

        let has_outputs = !snap.outputs.is_empty();
        let has_ec = snap.execution_count != "null" && !snap.execution_count.is_empty();

        if has_outputs || has_ec || attempt >= 4 {
            blob_base_url_out = blob_base_url.clone();
            blob_store_path_out = blob_store_path.clone();
            comms_out = handle.get_runtime_state().ok().map(|rs| rs.comms);
            snapshot = Some(snap);
            break;
        }

        drop(st);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }

    let snapshot = snapshot.ok_or_else(|| to_py_err("Failed to read cell after execution"))?;

    let execution_count = snapshot.execution_count.parse::<i64>().ok();

    let outputs = output_resolver::resolve_cell_outputs(
        &snapshot.outputs,
        &blob_base_url_out,
        &blob_store_path_out,
        comms_out.as_ref(),
    )
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

    let response = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle.confirm_sync().await.map_err(to_py_err)?;

        handle
            .send_request(NotebookRequest::RunAllCells {})
            .await
            .map_err(to_py_err)?
    };

    match response {
        NotebookResponse::AllCellsQueued { queued } => {
            let count = queued.len();
            // Focus on the last code cell — gives a visual anchor for where execution ends.
            // RunAllCells only queues code cells, so focusing the last code cell (not the
            // last cell overall, which might be markdown/raw) is more accurate.
            if count > 0 {
                let last_code_cell_id = {
                    let st = state.lock().await;
                    st.handle.as_ref().and_then(|h| {
                        let cells = h.get_cells();
                        cells
                            .iter()
                            .rev()
                            .find(|c| c.cell_type == "code")
                            .map(|c| c.id.clone())
                    })
                };
                // Don't emit focus — it would overwrite any existing cursor presence.
                let _ = last_code_cell_id;
            }
            Ok(count)
        }
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
    // Single lock: read actor_label and clone handle atomically so the
    // encoded actor_label always matches the session's current identity.
    let (data, handle) = {
        let st = state.lock().await;
        let data = notebook_doc::presence::encode_cursor_update_labeled(
            "local",
            peer_label,
            st.actor_label.as_deref(),
            &notebook_doc::presence::CursorPosition {
                cell_id: cell_id.to_string(),
                line,
                column,
            },
        );
        let handle = st
            .handle
            .clone()
            .ok_or_else(|| to_py_err("Not connected"))?;
        (data, handle)
    };
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
    let (data, handle) = {
        let st = state.lock().await;
        let data = notebook_doc::presence::encode_selection_update_labeled(
            "local",
            peer_label,
            st.actor_label.as_deref(),
            &notebook_doc::presence::SelectionRange {
                cell_id: cell_id.to_string(),
                anchor_line,
                anchor_col,
                head_line,
                head_col,
            },
        );
        let handle = st
            .handle
            .clone()
            .ok_or_else(|| to_py_err("Not connected"))?;
        (data, handle)
    };
    handle.send_presence(data).await.map_err(to_py_err)
}

/// Get all connected peer IDs and labels.
pub(crate) async fn get_peers(state: &Arc<Mutex<SessionState>>) -> PyResult<Vec<(String, String)>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;
    Ok(handle.get_peers())
}

/// Get remote peer cursors as (peer_id, peer_label, cell_id, line, column).
pub(crate) async fn get_remote_cursors(
    state: &Arc<Mutex<SessionState>>,
) -> PyResult<Vec<RemoteCursor>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;
    Ok(handle
        .remote_cursors("local")
        .into_iter()
        .map(|(id, label, pos)| (id, label, pos.cell_id, pos.line, pos.column))
        .collect())
}

/// Internal helper to emit cursor presence (best-effort).
/// Reads peer_label from SessionState, so callers don't need to pass it.
/// Errors are silently ignored since presence is non-critical.
pub(crate) async fn emit_cursor_presence(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    line: u32,
    column: u32,
) {
    // Best-effort: don't propagate errors
    let _ = emit_cursor_presence_internal(state, cell_id, line, column).await;
}

async fn emit_cursor_presence_internal(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    line: u32,
    column: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Clone what we need and drop the lock before await to avoid contention
    let (data, handle) = {
        let st = state.lock().await;
        let peer_label = st.peer_label.clone();
        let actor_label = st.actor_label.clone();
        let data = notebook_doc::presence::encode_cursor_update_labeled(
            "local",
            peer_label.as_deref(),
            actor_label.as_deref(),
            &notebook_doc::presence::CursorPosition {
                cell_id: cell_id.to_string(),
                line,
                column,
            },
        );
        let handle = st.handle.clone().ok_or("Not connected")?;
        (data, handle)
    };
    handle.send_presence(data).await?;
    Ok(())
}

/// Internal helper to emit focus presence (best-effort).
pub(crate) async fn emit_focus_presence(state: &Arc<Mutex<SessionState>>, cell_id: &str) {
    let _ = emit_focus_presence_internal(state, cell_id).await;
}

async fn emit_focus_presence_internal(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (data, handle) = {
        let st = state.lock().await;
        let peer_label = st.peer_label.clone();
        let actor_label = st.actor_label.clone();
        let data = notebook_doc::presence::encode_focus_update_labeled(
            "local",
            peer_label.as_deref(),
            actor_label.as_deref(),
            cell_id,
        );
        let handle = st.handle.clone().ok_or("Not connected")?;
        (data, handle)
    };
    handle.send_presence(data).await?;
    Ok(())
}

/// Internal helper to clear a presence channel (best-effort).
pub(crate) async fn emit_clear_channel(
    state: &Arc<Mutex<SessionState>>,
    channel: notebook_doc::presence::Channel,
) {
    let _ = emit_clear_channel_internal(state, channel).await;
}

async fn emit_clear_channel_internal(
    state: &Arc<Mutex<SessionState>>,
    channel: notebook_doc::presence::Channel,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (data, handle) = {
        let st = state.lock().await;
        let data = notebook_doc::presence::encode_clear_channel("local", channel);
        let handle = st.handle.clone().ok_or("Not connected")?;
        (data, handle)
    };
    handle.send_presence(data).await?;
    Ok(())
}

/// Set cell focus (presence dot without cursor position).
pub(crate) async fn set_focus(
    state: &Arc<Mutex<SessionState>>,
    peer_label: Option<&str>,
    cell_id: &str,
) -> PyResult<()> {
    let (data, handle) = {
        let st = state.lock().await;
        let data = notebook_doc::presence::encode_focus_update_labeled(
            "local",
            peer_label,
            st.actor_label.as_deref(),
            cell_id,
        );
        let handle = st
            .handle
            .clone()
            .ok_or_else(|| to_py_err("Not connected"))?;
        (data, handle)
    };
    handle.send_presence(data).await.map_err(to_py_err)
}

/// Clear cursor presence channel.
pub(crate) async fn clear_cursor(state: &Arc<Mutex<SessionState>>) -> PyResult<()> {
    let data = notebook_doc::presence::encode_clear_channel(
        "local",
        notebook_doc::presence::Channel::Cursor,
    );
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;
    handle.send_presence(data).await.map_err(to_py_err)
}

/// Clear selection presence channel.
pub(crate) async fn clear_selection(state: &Arc<Mutex<SessionState>>) -> PyResult<()> {
    let data = notebook_doc::presence::encode_clear_channel(
        "local",
        notebook_doc::presence::Channel::Selection,
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

    // Synchronous — direct doc mutation via DocHandle
    handle.set_metadata_string(key, value).map_err(to_py_err)?;
    Ok(())
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

    // Synchronous — read from doc via DocHandle
    Ok(handle.get_metadata_string(key))
}

// =========================================================================
// Low-level sync (for testing / cross-impl verification)
// =========================================================================

/// Export the raw Automerge document bytes from the local replica.
///
/// Calls `doc.save()` on the underlying `AutoCommit`, returning the full
/// serialized Automerge document. Useful for cross-implementation
/// compatibility testing (e.g., loading the bytes in WASM).
pub(crate) async fn get_automerge_doc_bytes(state: &Arc<Mutex<SessionState>>) -> PyResult<Vec<u8>> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.with_doc(|doc| doc.save()).map_err(to_py_err)
}

/// Confirm that the daemon has merged all local changes.
///
/// Blocks until the daemon's shared_heads include our local heads,
/// ensuring the daemon sees all local mutations (cell creates, source
/// updates, etc.) before proceeding.
pub(crate) async fn confirm_sync(state: &Arc<Mutex<SessionState>>) -> PyResult<()> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;

    handle.confirm_sync().await.map_err(to_py_err)
}

/// Save the notebook to disk.
/// Result of a save operation, containing the saved path and optionally
/// a new notebook_id if the daemon re-keyed the room (ephemeral → file-path).
pub(crate) struct SaveResult {
    pub path: String,
    pub new_notebook_id: Option<String>,
}

pub(crate) async fn save(
    state: &Arc<Mutex<SessionState>>,
    path: Option<&str>,
) -> PyResult<SaveResult> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .clone()
            .ok_or_else(|| to_py_err("Not connected"))?
    };

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
        NotebookResponse::NotebookSaved {
            path: saved_path,
            new_notebook_id,
        } => Ok(SaveResult {
            path: saved_path,
            new_notebook_id,
        }),
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
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?
            .clone()
    };

    match handle.get_cell_metadata(cell_id) {
        Some(metadata) => Ok(Some(
            serde_json::to_string(&metadata).map_err(|e| to_py_err(format!("Serialize: {}", e)))?,
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

    // Synchronous — direct doc mutation via DocHandle
    handle
        .set_cell_metadata(cell_id, &metadata)
        .map_err(to_py_err)
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

    // Synchronous — direct doc mutation via DocHandle
    handle
        .update_cell_metadata_at(cell_id, &path_refs, value)
        .map_err(to_py_err)
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

    // Synchronous — read from watch snapshot via DocHandle
    Ok(handle.get_notebook_metadata().unwrap_or_default())
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

    // Synchronous — direct doc mutation via DocHandle
    handle.set_metadata_snapshot(snapshot).map_err(to_py_err)?;
    Ok(())
}

/// Sync environment with current metadata and poll for completion.
pub(crate) async fn sync_environment_impl(
    state: &Arc<Mutex<SessionState>>,
) -> PyResult<SyncEnvironmentResult> {
    let response = {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;

        handle
            .send_request(NotebookRequest::SyncEnvironment {})
            .await
            .map_err(to_py_err)?
    };

    match response {
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
            n,
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

/// Get the full runtime state from the local Automerge replica (no daemon round-trip).
pub(crate) async fn get_runtime_state(
    state: &Arc<Mutex<SessionState>>,
) -> PyResult<PyRuntimeState> {
    let st = state.lock().await;
    let handle = st
        .handle
        .as_ref()
        .ok_or_else(|| to_py_err("Not connected"))?;
    let rs = handle
        .get_runtime_state()
        .map_err(|e| to_py_err(format!("{}", e)))?;
    Ok(rs.into())
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
        NotebookResponse::QueueState { executing, queued } => Ok(QueueState {
            executing: executing.map(|e| PyQueueEntry {
                cell_id: e.cell_id,
                execution_id: e.execution_id,
            }),
            queued: queued
                .into_iter()
                .map(|e| PyQueueEntry {
                    cell_id: e.cell_id,
                    execution_id: e.execution_id,
                })
                .collect(),
        }),
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

    let runtime = {
        let st = state.lock().await;
        st.runtime.clone()
    };
    start_kernel(state, &runtime, "auto", None).await
}

/// Resolve blob server URL and store path from daemon info.
async fn resolve_blob_paths(socket_path: &Path) -> (Option<String>, Option<PathBuf>) {
    if let Some(parent) = socket_path.parent() {
        let daemon_json = parent.join("daemon.json");
        let base_url = if daemon_json.exists() {
            tokio::fs::read_to_string(&daemon_json)
                .await
                .ok()
                .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
                .and_then(|info| info.get("blob_port").and_then(|p| p.as_u64()))
                .map(|port| format!("http://localhost:{}", port))
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

// =========================================================================
// Env progress formatting (moved from subscription.rs)
// =========================================================================

use kernel_env::EnvProgressPhase;

fn env_progress_message(phase: &EnvProgressPhase) -> String {
    match phase {
        EnvProgressPhase::Starting { .. } => "Preparing environment...".to_string(),
        EnvProgressPhase::CacheHit { .. } => "Using cached environment".to_string(),
        EnvProgressPhase::LockFileHit => "Rebuilding from lock file".to_string(),
        EnvProgressPhase::OfflineHit => "Using cached packages (offline)".to_string(),
        EnvProgressPhase::FetchingRepodata { channels } => {
            format!("Fetching package index ({})", channels.join(", "))
        }
        EnvProgressPhase::RepodataComplete { record_count, .. } => {
            format!("Loaded {} packages", record_count)
        }
        EnvProgressPhase::Solving { spec_count } => {
            format!("Solving dependencies ({} specs)", spec_count)
        }
        EnvProgressPhase::SolveComplete { package_count, .. } => {
            format!("Resolved {} packages", package_count)
        }
        EnvProgressPhase::Installing { total } => format!("Installing {} packages...", total),
        EnvProgressPhase::DownloadProgress {
            completed,
            total,
            current_package,
            bytes_per_second,
            ..
        } => {
            let speed = format_bytes_per_sec(*bytes_per_second);
            format!(
                "Downloading {}/{} {} @ {}",
                completed, total, current_package, speed
            )
        }
        EnvProgressPhase::LinkProgress {
            completed,
            total,
            current_package,
        } => format!("Installing {}/{} {}", completed, total, current_package),
        EnvProgressPhase::InstallComplete { .. } => "Installation complete".to_string(),
        EnvProgressPhase::CreatingVenv => "Creating virtual environment...".to_string(),
        EnvProgressPhase::InstallingPackages { packages } => {
            format!("Installing {} packages...", packages.len())
        }
        EnvProgressPhase::Ready { .. } => "Environment ready".to_string(),
        EnvProgressPhase::Error { message } => format!("Environment error: {}", message),
    }
}

fn format_bytes_per_sec(bps: f64) -> String {
    if bps >= 1_048_576.0 {
        format!("{:.1} MiB/s", bps / 1_048_576.0)
    } else if bps >= 1024.0 {
        format!("{:.1} KiB/s", bps / 1024.0)
    } else {
        format!("{:.0} B/s", bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_actor_label_format() {
        let label = make_actor_label("Claude");
        assert!(label.starts_with("agent:claude:"), "got: {}", label);
        // Session suffix should be 8 hex chars
        let suffix = label.strip_prefix("agent:claude:").unwrap();
        assert_eq!(suffix.len(), 8, "suffix should be 8 chars: {}", suffix);
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex: {}",
            suffix
        );
    }

    #[test]
    fn test_make_actor_label_lowercases() {
        let label = make_actor_label("Codex");
        assert!(label.starts_with("agent:codex:"));
    }

    #[test]
    fn test_make_actor_label_unique() {
        let a = make_actor_label("Claude");
        let b = make_actor_label("Claude");
        assert_ne!(a, b, "each call should produce a unique session suffix");
    }

    /// Regression test: spawn_rekey_watcher must work when called OUTSIDE
    /// of `runtime.block_on()` — the sync Session API calls it after
    /// `block_on` returns. Previously it used `tokio::spawn` which panics
    /// without an active runtime context. The fix uses `handle.spawn()`.
    #[test]
    fn test_spawn_rekey_watcher_outside_block_on() {
        use notebook_protocol::protocol::NotebookBroadcast;

        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Create a broadcast channel (capacity 16 is fine for tests)
        let (tx, rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(16);
        let broadcast_rx = notebook_sync::BroadcastReceiver::new(rx);

        let override_arc = Arc::new(std::sync::Mutex::new(None::<String>));

        // This is the critical call — it must NOT panic when called
        // outside of block_on, using only the runtime handle.
        spawn_rekey_watcher(&broadcast_rx, Arc::clone(&override_arc), runtime.handle());

        // Send a RoomRenamed broadcast and verify the override is updated
        let new_id = "notebooks/test.ipynb".to_string();
        tx.send(NotebookBroadcast::RoomRenamed {
            new_notebook_id: new_id.clone(),
        })
        .unwrap();

        // Give the spawned task a moment to process
        runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let override_val = override_arc.lock().unwrap();
        assert_eq!(
            override_val.as_deref(),
            Some("notebooks/test.ipynb"),
            "spawn_rekey_watcher should update notebook_id_override on RoomRenamed"
        );
    }

    /// Helper that mirrors the restart env_source mapping logic in restart_kernel().
    fn restart_env_source(prev: Option<&str>) -> String {
        match prev {
            Some("uv:prewarmed") => "auto:uv".to_string(),
            Some("conda:prewarmed") => "auto:conda".to_string(),
            None => "auto".to_string(),
            Some(s) => s.to_string(),
        }
    }

    #[test]
    fn test_restart_env_source_uv_prewarmed() {
        assert_eq!(restart_env_source(Some("uv:prewarmed")), "auto:uv");
    }

    #[test]
    fn test_restart_env_source_conda_prewarmed() {
        assert_eq!(restart_env_source(Some("conda:prewarmed")), "auto:conda");
    }

    #[test]
    fn test_restart_env_source_none_defaults_to_unscoped_auto() {
        assert_eq!(restart_env_source(None), "auto");
    }

    #[test]
    fn test_restart_env_source_explicit_passes_through() {
        assert_eq!(restart_env_source(Some("uv:inline")), "uv:inline");
        assert_eq!(restart_env_source(Some("conda:inline")), "conda:inline");
        assert_eq!(restart_env_source(Some("uv:pyproject")), "uv:pyproject");
    }
}
