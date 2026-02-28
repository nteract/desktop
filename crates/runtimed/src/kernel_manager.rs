//! Daemon-owned kernel management for notebook rooms.
//!
//! Each notebook room can have one kernel. The daemon owns the kernel lifecycle
//! and execution queue, broadcasting outputs to all connected peers.
//!
//! This replaces the notebook app's local kernel management for Phase 8:
//! - Execute requests flow through the daemon
//! - Daemon tracks msg_id → cell_id perfectly
//! - Outputs broadcast to all windows showing the same notebook

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use bytes::Bytes;
use jupyter_protocol::{
    ConnectionInfo, ExecuteRequest, HistoryRequest, InterruptRequest, JupyterMessage,
    JupyterMessageContent, KernelInfoRequest, ShutdownRequest,
};
use log::{debug, error, info, warn};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};
use uuid::Uuid;

use crate::blob_store::BlobStore;
use crate::comm_state::CommState;
use crate::notebook_doc::NotebookDoc;
use crate::notebook_sync_server::persist_notebook_bytes;
use crate::output_store::{self, DEFAULT_INLINE_THRESHOLD};
use crate::protocol::{HistoryEntry, NotebookBroadcast};
use crate::PooledEnv;

/// Convert a JupyterMessageContent to nbformat-style JSON for storage in Automerge.
///
/// jupyter_protocol serializes as: `{"ExecuteResult": {"data": {...}, ...}}`
/// nbformat expects: `{"output_type": "execute_result", "data": {...}, ...}`
fn message_content_to_nbformat(content: &JupyterMessageContent) -> Option<serde_json::Value> {
    use serde_json::json;

    match content {
        JupyterMessageContent::StreamContent(stream) => {
            let name = match stream.name {
                jupyter_protocol::Stdio::Stdout => "stdout",
                jupyter_protocol::Stdio::Stderr => "stderr",
            };
            Some(json!({
                "output_type": "stream",
                "name": name,
                "text": stream.text
            }))
        }
        JupyterMessageContent::DisplayData(data) => {
            let mut output = json!({
                "output_type": "display_data",
                "data": data.data,
                "metadata": data.metadata
            });
            // Preserve display_id for update_display_data targeting
            if let Some(ref transient) = data.transient {
                if let Some(ref display_id) = transient.display_id {
                    output["transient"] = json!({ "display_id": display_id });
                }
            }
            Some(output)
        }
        JupyterMessageContent::ExecuteResult(result) => Some(json!({
            "output_type": "execute_result",
            "data": result.data,
            "metadata": result.metadata,
            "execution_count": result.execution_count.0
        })),
        JupyterMessageContent::ErrorOutput(error) => Some(json!({
            "output_type": "error",
            "ename": error.ename,
            "evalue": error.evalue,
            "traceback": error.traceback
        })),
        _ => None,
    }
}

/// Convert a Jupyter Media bundle (from page payload) to nbformat display_data JSON.
///
/// Page payloads are used by IPython for `?` and `??` help. This converts
/// them to display_data outputs so help content appears in cell outputs.
fn media_to_display_data(media: &jupyter_protocol::Media) -> serde_json::Value {
    serde_json::json!({
        "output_type": "display_data",
        "data": media,
        "metadata": {}
    })
}

/// Check if a string looks like a manifest hash (64-char hex).
fn is_manifest_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Update an output by display_id when outputs are manifest hashes.
///
/// This function iterates through all cells and outputs in the document,
/// looking for a manifest with a matching display_id. When found, it creates
/// a new manifest with updated data and replaces the hash in the document.
///
/// Returns true if an output was found and updated, false otherwise.
async fn update_output_by_display_id_with_manifests(
    doc: &mut NotebookDoc,
    display_id: &str,
    new_data: &serde_json::Value,
    new_metadata: &serde_json::Map<String, serde_json::Value>,
    blob_store: &BlobStore,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Get all outputs from the document
    let outputs = doc.get_all_outputs();

    for (cell_id, output_idx, output_str) in outputs {
        // Check if it's a manifest hash or raw JSON
        if is_manifest_hash(&output_str) {
            // Fetch manifest from blob store
            let manifest_bytes = match blob_store.get(&output_str).await? {
                Some(bytes) => bytes,
                None => continue,
            };
            let manifest_json = String::from_utf8(manifest_bytes)?;

            // Try to update the manifest
            if let Some(updated_manifest) = output_store::update_manifest_display_data(
                &manifest_json,
                display_id,
                new_data,
                new_metadata,
                blob_store,
                DEFAULT_INLINE_THRESHOLD,
            )
            .await?
            {
                // Store the updated manifest and get new hash
                let new_hash = output_store::store_manifest(&updated_manifest, blob_store).await?;

                // Replace the hash in the document
                doc.replace_output(&cell_id, output_idx, &new_hash)?;
                return Ok(true);
            }
        } else {
            // Backward compatibility: try parsing as raw JSON
            let mut output_json: serde_json::Value = match serde_json::from_str(&output_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let matches = output_json
                .get("transient")
                .and_then(|t| t.get("display_id"))
                .and_then(|d| d.as_str())
                == Some(display_id);

            if matches {
                // Update data and metadata in place
                output_json["data"] = new_data.clone();
                output_json["metadata"] = serde_json::Value::Object(new_metadata.clone());

                // Write back
                let updated_str = output_json.to_string();
                doc.replace_output(&cell_id, output_idx, &updated_str)?;
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// A cell queued for execution.
#[derive(Debug, Clone)]
pub struct QueuedCell {
    pub cell_id: String,
    pub code: String,
}

/// Kernel status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelStatus {
    /// Kernel is starting up
    Starting,
    /// Kernel is ready and idle
    Idle,
    /// Kernel is executing code
    Busy,
    /// Kernel encountered an error
    Error,
    /// Kernel is shutting down
    ShuttingDown,
}

impl std::fmt::Display for KernelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelStatus::Starting => write!(f, "starting"),
            KernelStatus::Idle => write!(f, "idle"),
            KernelStatus::Busy => write!(f, "busy"),
            KernelStatus::Error => write!(f, "error"),
            KernelStatus::ShuttingDown => write!(f, "shutdown"),
        }
    }
}

/// A kernel owned by the daemon for a notebook room.
///
/// Unlike the notebook app's `NotebookKernel`, this broadcasts outputs
/// to all connected peers rather than emitting Tauri events.
pub struct RoomKernel {
    /// Kernel type (e.g., "python", "deno")
    kernel_type: String,
    /// Environment source (e.g., "uv:inline", "conda:prewarmed")
    env_source: String,
    /// Connection info for the kernel
    connection_info: Option<ConnectionInfo>,
    /// Path to the connection file
    connection_file: Option<PathBuf>,
    /// Session ID for Jupyter protocol
    session_id: String,
    /// Handle to the iopub listener task
    iopub_task: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the shell reader task
    shell_reader_task: Option<tokio::task::JoinHandle<()>>,
    /// Shell writer for sending execute requests
    shell_writer: Option<runtimelib::DealerSendConnection>,
    /// The kernel process
    process: Option<tokio::process::Child>,
    /// Process group ID for cleanup (Unix only)
    #[cfg(unix)]
    process_group_id: Option<i32>,
    /// Mapping from msg_id → cell_id for routing iopub messages
    cell_id_map: Arc<StdMutex<HashMap<String, String>>>,
    /// Execution queue (pending cells)
    queue: VecDeque<QueuedCell>,
    /// Currently executing cell
    executing: Option<String>,
    /// Current kernel status
    status: KernelStatus,
    /// Broadcast channel for sending outputs to peers
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Command sender for iopub/shell tasks
    cmd_tx: Option<mpsc::Sender<QueueCommand>>,
    /// Command receiver for queue state updates (polled by sync server)
    cmd_rx: Option<mpsc::Receiver<QueueCommand>>,
    /// Automerge document for persisting outputs
    doc: Arc<RwLock<NotebookDoc>>,
    /// Path for persisting the document
    persist_path: PathBuf,
    /// Channel to notify peers of document changes
    changed_tx: broadcast::Sender<()>,
    /// Blob store for output manifests
    blob_store: Arc<BlobStore>,
    /// Comm state for widget synchronization across windows
    comm_state: Arc<CommState>,
    /// Pending history requests: msg_id → response channel
    pending_history: Arc<StdMutex<HashMap<String, oneshot::Sender<Vec<HistoryEntry>>>>>,
}

/// Commands from iopub/shell handlers for queue state management.
///
/// These are sent from spawned tasks and must be processed by code
/// that has access to `&mut RoomKernel` (e.g., the notebook sync server).
#[derive(Debug)]
pub enum QueueCommand {
    /// A cell finished executing (received status=idle from kernel)
    ExecutionDone { cell_id: String },
    /// A cell produced an error (for stop-on-error behavior)
    CellError { cell_id: String },
}

impl RoomKernel {
    /// Create a new room kernel with a broadcast channel for outputs.
    pub fn new(
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
        doc: Arc<RwLock<NotebookDoc>>,
        persist_path: PathBuf,
        changed_tx: broadcast::Sender<()>,
        blob_store: Arc<BlobStore>,
        comm_state: Arc<CommState>,
    ) -> Self {
        Self {
            kernel_type: String::new(),
            env_source: String::new(),
            connection_info: None,
            connection_file: None,
            session_id: Uuid::new_v4().to_string(),
            iopub_task: None,
            shell_reader_task: None,
            shell_writer: None,
            process: None,
            #[cfg(unix)]
            process_group_id: None,
            cell_id_map: Arc::new(StdMutex::new(HashMap::new())),
            queue: VecDeque::new(),
            executing: None,
            status: KernelStatus::Starting,
            broadcast_tx,
            cmd_tx: None,
            cmd_rx: None,
            doc,
            persist_path,
            changed_tx,
            blob_store,
            comm_state,
            pending_history: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Take the command receiver for polling by the sync server.
    ///
    /// This should be called after `launch()` and polled in the sync server's
    /// select loop. When commands arrive, call the appropriate methods on
    /// `RoomKernel` (e.g., `execution_done` for `ExecutionDone`).
    pub fn take_command_rx(&mut self) -> Option<mpsc::Receiver<QueueCommand>> {
        self.cmd_rx.take()
    }

    /// Get the kernel type.
    pub fn kernel_type(&self) -> &str {
        &self.kernel_type
    }

    /// Get the environment source.
    pub fn env_source(&self) -> &str {
        &self.env_source
    }

    /// Get the current kernel status.
    pub fn status(&self) -> KernelStatus {
        self.status
    }

    /// Check if the kernel is running.
    pub fn is_running(&self) -> bool {
        self.shell_writer.is_some()
    }

    /// Get the currently executing cell ID.
    pub fn executing_cell(&self) -> Option<&String> {
        self.executing.as_ref()
    }

    /// Get the queued cell IDs.
    pub fn queued_cells(&self) -> Vec<String> {
        self.queue.iter().map(|c| c.cell_id.clone()).collect()
    }

    /// Launch a kernel for this room.
    ///
    /// For Python kernels, uses the provided pooled environment.
    /// For non-Python kernels (e.g., Deno), falls back to kernelspec discovery.
    pub async fn launch(
        &mut self,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&std::path::Path>,
        env: Option<PooledEnv>,
    ) -> Result<()> {
        // Shutdown existing kernel if any (but don't broadcast shutdown for fresh kernel)
        if self.is_running() {
            self.shutdown().await.ok();
        }

        self.kernel_type = kernel_type.to_string();
        self.env_source = env_source.to_string();
        self.status = KernelStatus::Starting;

        // Broadcast starting status
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "starting".to_string(),
            cell_id: None,
        });

        // Determine kernel name for connection info
        let kernelspec_name = match kernel_type {
            "python" => "python3",
            "deno" => "deno",
            _ => kernel_type,
        };

        // Reserve ports
        let ip = std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let ports = runtimelib::peek_ports(ip, 5).await?;

        let connection_info = ConnectionInfo {
            transport: jupyter_protocol::connection_info::Transport::TCP,
            ip: ip.to_string(),
            stdin_port: ports[0],
            control_port: ports[1],
            hb_port: ports[2],
            shell_port: ports[3],
            iopub_port: ports[4],
            signature_scheme: "hmac-sha256".to_string(),
            key: Uuid::new_v4().to_string(),
            kernel_name: Some(kernelspec_name.to_string()),
        };

        // Write connection file
        let runtime_dir = runtimelib::dirs::runtime_dir();
        tokio::fs::create_dir_all(&runtime_dir).await?;

        let kernel_id: String =
            petname::petname(2, "-").unwrap_or_else(|| Uuid::new_v4().to_string());
        let connection_file_path = runtime_dir.join(format!("runtimed-kernel-{}.json", kernel_id));

        tokio::fs::write(
            &connection_file_path,
            serde_json::to_string_pretty(&connection_info)?,
        )
        .await?;

        // Determine working directory
        let cwd = if let Some(path) = notebook_path {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(std::env::temp_dir)
        } else {
            // For untitled notebooks, use home directory (which always exists)
            dirs::home_dir().unwrap_or_else(std::env::temp_dir)
        };

        // Build kernel command based on kernel type and environment
        let mut cmd = if kernel_type == "python" {
            if let Some(ref pooled_env) = env {
                // Python with pooled environment - use the env's Python directly
                info!(
                    "[kernel-manager] Starting Python kernel from pooled env at {:?}",
                    pooled_env.python_path
                );
                let mut c = tokio::process::Command::new(&pooled_env.python_path);
                c.args(["-Xfrozen_modules=off", "-m", "ipykernel_launcher", "-f"]);
                c.arg(&connection_file_path);
                c.stdout(Stdio::null());
                c.stderr(Stdio::null());
                c
            } else {
                // Python without pooled env - shouldn't happen, but fall back to kernelspec
                warn!(
                    "[kernel-manager] No pooled env provided for Python kernel, falling back to kernelspec"
                );
                let kernelspec = runtimelib::find_kernelspec(kernelspec_name).await?;
                kernelspec.command(
                    &connection_file_path,
                    Some(Stdio::null()),
                    Some(Stdio::null()),
                )?
            }
        } else {
            // Non-Python kernels (e.g., Deno) - use kernelspec discovery
            info!(
                "[kernel-manager] Starting {} kernel via kernelspec at {:?}",
                kernel_type, connection_file_path
            );
            let kernelspec = runtimelib::find_kernelspec(kernelspec_name).await?;
            kernelspec.command(
                &connection_file_path,
                Some(Stdio::null()),
                Some(Stdio::null()),
            )?
        };
        cmd.current_dir(&cwd);

        #[cfg(unix)]
        cmd.process_group(0);

        let process = cmd.kill_on_drop(true).spawn()?;

        #[cfg(unix)]
        {
            self.process_group_id = process.id().map(|pid| pid as i32);
        }

        // Small delay to let the kernel start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        self.session_id = Uuid::new_v4().to_string();

        // Create iopub connection and spawn listener
        let mut iopub =
            runtimelib::create_client_iopub_connection(&connection_info, "", &self.session_id)
                .await?;

        // Create command channel for queue processing
        let (cmd_tx, cmd_rx) = mpsc::channel::<QueueCommand>(100);
        self.cmd_tx = Some(cmd_tx.clone());

        let broadcast_tx = self.broadcast_tx.clone();
        let cell_id_map = self.cell_id_map.clone();
        let iopub_cmd_tx = cmd_tx.clone();
        let doc = self.doc.clone();
        let persist_path = self.persist_path.clone();
        let changed_tx = self.changed_tx.clone();
        let blob_store = self.blob_store.clone();
        let comm_state = self.comm_state.clone();

        let iopub_task = tokio::spawn(async move {
            loop {
                match iopub.read().await {
                    Ok(message) => {
                        debug!(
                            "[kernel-manager] iopub: type={} parent_msg_id={:?}",
                            message.header.msg_type,
                            message.parent_header.as_ref().map(|h| &h.msg_id)
                        );

                        // Look up cell_id from msg_id
                        let cell_id = message
                            .parent_header
                            .as_ref()
                            .and_then(|h| cell_id_map.lock().ok()?.get(&h.msg_id).cloned());

                        // Handle different message types
                        match &message.content {
                            JupyterMessageContent::Status(status) => {
                                let status_str = match status.execution_state {
                                    jupyter_protocol::ExecutionState::Busy => "busy",
                                    jupyter_protocol::ExecutionState::Idle => "idle",
                                    jupyter_protocol::ExecutionState::Starting => "starting",
                                    jupyter_protocol::ExecutionState::Restarting => "restarting",
                                    jupyter_protocol::ExecutionState::Terminating
                                    | jupyter_protocol::ExecutionState::Dead => "shutdown",
                                    _ => "unknown",
                                };

                                let _ = broadcast_tx.send(NotebookBroadcast::KernelStatus {
                                    status: status_str.to_string(),
                                    cell_id: cell_id.clone(),
                                });

                                // Signal execution done when idle
                                if status.execution_state == jupyter_protocol::ExecutionState::Idle
                                {
                                    if let Some(cid) = cell_id {
                                        let _ = iopub_cmd_tx
                                            .try_send(QueueCommand::ExecutionDone { cell_id: cid });
                                    }
                                }
                            }

                            JupyterMessageContent::ExecuteInput(input) => {
                                if let Some(ref cid) = cell_id {
                                    let _ =
                                        broadcast_tx.send(NotebookBroadcast::ExecutionStarted {
                                            cell_id: cid.clone(),
                                            execution_count: input.execution_count.0 as i64,
                                        });
                                }
                            }

                            JupyterMessageContent::StreamContent(_)
                            | JupyterMessageContent::DisplayData(_)
                            | JupyterMessageContent::ExecuteResult(_) => {
                                if let Some(ref cid) = cell_id {
                                    let output_type = match &message.content {
                                        JupyterMessageContent::StreamContent(_) => "stream",
                                        JupyterMessageContent::DisplayData(_) => "display_data",
                                        JupyterMessageContent::ExecuteResult(_) => "execute_result",
                                        _ => "unknown",
                                    };

                                    // Convert to nbformat JSON for storage
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Create manifest (inlines small data, blobs large data)
                                        let output_ref = match output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            Ok(manifest_json) => {
                                                // Store manifest in blob store, get hash
                                                match output_store::store_manifest(
                                                    &manifest_json,
                                                    &blob_store,
                                                )
                                                .await
                                                {
                                                    Ok(hash) => hash,
                                                    Err(e) => {
                                                        warn!(
                                                            "[kernel-manager] Failed to store manifest: {}",
                                                            e
                                                        );
                                                        nbformat_value.to_string()
                                                        // Fallback to raw JSON
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to create manifest: {}",
                                                    e
                                                );
                                                nbformat_value.to_string() // Fallback to raw JSON
                                            }
                                        };

                                        // Append hash (or fallback JSON) to Automerge doc
                                        let persist_bytes = {
                                            let mut doc_guard = doc.write().await;
                                            if let Err(e) =
                                                doc_guard.append_output(cid, &output_ref)
                                            {
                                                warn!(
                                                    "[kernel-manager] Failed to append output to doc: {}",
                                                    e
                                                );
                                            }
                                            let bytes = doc_guard.save();
                                            let _ = changed_tx.send(());
                                            bytes
                                        };
                                        persist_notebook_bytes(&persist_bytes, &persist_path);

                                        let _ = broadcast_tx.send(NotebookBroadcast::Output {
                                            cell_id: cid.clone(),
                                            output_type: output_type.to_string(),
                                            output_json: output_ref,
                                        });
                                    }
                                }
                            }

                            // UpdateDisplayData mutates an existing output in place (e.g., progress bars).
                            // Find the output by display_id and update it, rather than appending.
                            // Supports both manifest hashes and raw JSON (backward compatibility).
                            JupyterMessageContent::UpdateDisplayData(update) => {
                                if let Some(ref display_id) = update.transient.display_id {
                                    let persist_bytes = {
                                        let mut doc_guard = doc.write().await;
                                        match update_output_by_display_id_with_manifests(
                                            &mut doc_guard,
                                            display_id,
                                            &serde_json::to_value(&update.data).unwrap_or_default(),
                                            &update.metadata,
                                            &blob_store,
                                        )
                                        .await
                                        {
                                            Ok(true) => {
                                                debug!(
                                                    "[kernel-manager] Updated display_id={}",
                                                    display_id
                                                );
                                            }
                                            Ok(false) => {
                                                warn!(
                                                    "[kernel-manager] No output found for display_id={}",
                                                    display_id
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to update display: {}",
                                                    e
                                                );
                                            }
                                        }
                                        let bytes = doc_guard.save();
                                        let _ = changed_tx.send(());
                                        bytes
                                    };
                                    persist_notebook_bytes(&persist_bytes, &persist_path);

                                    // Broadcast for immediate UI update
                                    // Frontend will receive via Automerge sync, but broadcast for speed
                                    let _ = broadcast_tx.send(NotebookBroadcast::DisplayUpdate {
                                        display_id: display_id.clone(),
                                        data: serde_json::to_value(&update.data)
                                            .unwrap_or_default(),
                                        metadata: update.metadata.clone(),
                                    });
                                }
                            }

                            JupyterMessageContent::ErrorOutput(_) => {
                                if let Some(ref cid) = cell_id {
                                    // Convert error to nbformat JSON
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Create manifest for error output
                                        let output_ref = match output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            Ok(manifest_json) => {
                                                match output_store::store_manifest(
                                                    &manifest_json,
                                                    &blob_store,
                                                )
                                                .await
                                                {
                                                    Ok(hash) => hash,
                                                    Err(e) => {
                                                        warn!(
                                                            "[kernel-manager] Failed to store error manifest: {}",
                                                            e
                                                        );
                                                        nbformat_value.to_string()
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to create error manifest: {}",
                                                    e
                                                );
                                                nbformat_value.to_string()
                                            }
                                        };

                                        // Write error output to Automerge doc before broadcasting
                                        let persist_bytes = {
                                            let mut doc_guard = doc.write().await;
                                            if let Err(e) =
                                                doc_guard.append_output(cid, &output_ref)
                                            {
                                                warn!(
                                                    "[kernel-manager] Failed to append error output to doc: {}",
                                                    e
                                                );
                                            }
                                            let bytes = doc_guard.save();
                                            let _ = changed_tx.send(());
                                            bytes
                                        };
                                        persist_notebook_bytes(&persist_bytes, &persist_path);

                                        let _ = broadcast_tx.send(NotebookBroadcast::Output {
                                            cell_id: cid.clone(),
                                            output_type: "error".to_string(),
                                            output_json: output_ref,
                                        });
                                    }

                                    // Signal cell error for stop-on-error
                                    let _ = iopub_cmd_tx.try_send(QueueCommand::CellError {
                                        cell_id: cid.clone(),
                                    });
                                }
                            }

                            // Comm messages for widgets (ipywidgets protocol)
                            JupyterMessageContent::CommOpen(open) => {
                                debug!(
                                    "[kernel-manager] Broadcasting comm_open: comm_id={}",
                                    open.comm_id.0
                                );

                                // Serialize the content to JSON
                                let content =
                                    serde_json::to_value(&message.content).unwrap_or_default();

                                // Extract buffers (Vec<Bytes> -> Vec<Vec<u8>>)
                                let buffers: Vec<Vec<u8>> =
                                    message.buffers.iter().map(|b| b.to_vec()).collect();

                                // Track comm state for multi-window sync
                                let data = serde_json::to_value(&open.data).unwrap_or_default();
                                comm_state
                                    .on_comm_open(
                                        &open.comm_id.0,
                                        &open.target_name,
                                        &data,
                                        buffers.clone(),
                                    )
                                    .await;

                                let _ = broadcast_tx.send(NotebookBroadcast::Comm {
                                    msg_type: message.header.msg_type.clone(),
                                    content,
                                    buffers,
                                });
                            }

                            JupyterMessageContent::CommMsg(msg) => {
                                debug!(
                                    "[kernel-manager] Broadcasting comm_msg: comm_id={}",
                                    msg.comm_id.0
                                );

                                // Serialize the content to JSON
                                let content =
                                    serde_json::to_value(&message.content).unwrap_or_default();

                                // Extract buffers (Vec<Bytes> -> Vec<Vec<u8>>)
                                let buffers: Vec<Vec<u8>> =
                                    message.buffers.iter().map(|b| b.to_vec()).collect();

                                // Track state updates (method="update") for multi-window sync
                                let data = serde_json::to_value(&msg.data).unwrap_or_default();
                                if data.get("method").and_then(|m| m.as_str()) == Some("update") {
                                    if let Some(state) = data.get("state") {
                                        comm_state.on_comm_update(&msg.comm_id.0, state).await;
                                    }
                                }

                                let _ = broadcast_tx.send(NotebookBroadcast::Comm {
                                    msg_type: message.header.msg_type.clone(),
                                    content,
                                    buffers,
                                });
                            }

                            JupyterMessageContent::CommClose(close) => {
                                debug!(
                                    "[kernel-manager] Broadcasting comm_close: comm_id={}",
                                    close.comm_id.0
                                );

                                // Serialize the content to JSON
                                let content =
                                    serde_json::to_value(&message.content).unwrap_or_default();

                                // Remove from comm state
                                comm_state.on_comm_close(&close.comm_id.0).await;

                                let _ = broadcast_tx.send(NotebookBroadcast::Comm {
                                    msg_type: message.header.msg_type.clone(),
                                    content,
                                    buffers: vec![],
                                });
                            }

                            _ => {
                                debug!(
                                    "[kernel-manager] Unhandled iopub message: {}",
                                    message.header.msg_type
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!("[kernel-manager] iopub read error: {}", e);
                        break;
                    }
                }
            }
        });

        // Create shell connection
        let identity = runtimelib::peer_identity_for_session(&self.session_id)?;
        let mut shell = runtimelib::create_client_shell_connection_with_identity(
            &connection_info,
            &self.session_id,
            identity,
        )
        .await?;

        // Verify kernel is alive
        let request: JupyterMessage = KernelInfoRequest::default().into();
        shell.send(request).await?;

        let reply = tokio::time::timeout(std::time::Duration::from_secs(30), shell.read()).await;
        match reply {
            Ok(Ok(msg)) => {
                info!(
                    "[kernel-manager] Kernel alive: got {} reply",
                    msg.header.msg_type
                );
            }
            Ok(Err(e)) => {
                error!("[kernel-manager] Error reading kernel_info_reply: {}", e);
                return Err(anyhow::anyhow!("Kernel did not respond: {}", e));
            }
            Err(_) => {
                error!("[kernel-manager] Timeout waiting for kernel_info_reply");
                return Err(anyhow::anyhow!("Kernel did not respond within 30s"));
            }
        }

        // Split shell into reader/writer
        let (shell_writer, mut shell_reader) = shell.split();

        // Spawn shell reader task
        let shell_broadcast_tx = self.broadcast_tx.clone();
        let shell_cell_id_map = self.cell_id_map.clone();
        let shell_pending_history = self.pending_history.clone();
        // Additional resources for handling page payloads (IPython ? and ?? help)
        let shell_doc = self.doc.clone();
        let shell_blob_store = self.blob_store.clone();
        let shell_persist_path = self.persist_path.clone();
        let shell_changed_tx = self.changed_tx.clone();

        let shell_reader_task = tokio::spawn(async move {
            loop {
                match shell_reader.read().await {
                    Ok(msg) => {
                        let _parent_msg_id = msg.parent_header.as_ref().map(|h| h.msg_id.clone());

                        match msg.content {
                            JupyterMessageContent::ExecuteReply(ref reply) => {
                                // Get cell_id from msg_id mapping
                                let cell_id = msg.parent_header.as_ref().and_then(|h| {
                                    shell_cell_id_map.lock().ok()?.get(&h.msg_id).cloned()
                                });

                                // Process page payloads - convert to display_data outputs
                                // This handles IPython's ? and ?? help commands
                                if let Some(ref cid) = cell_id {
                                    for payload in &reply.payload {
                                        if let jupyter_protocol::Payload::Page { data, .. } =
                                            payload
                                        {
                                            // Convert Media to nbformat display_data
                                            let nbformat_value = media_to_display_data(data);

                                            // Create manifest and store (same pattern as iopub_task)
                                            let output_ref = match output_store::create_manifest(
                                                &nbformat_value,
                                                &shell_blob_store,
                                                DEFAULT_INLINE_THRESHOLD,
                                            )
                                            .await
                                            {
                                                Ok(manifest_json) => {
                                                    match output_store::store_manifest(
                                                        &manifest_json,
                                                        &shell_blob_store,
                                                    )
                                                    .await
                                                    {
                                                        Ok(hash) => hash,
                                                        Err(e) => {
                                                            warn!(
                                                                "[kernel-manager] Failed to store page manifest: {}",
                                                                e
                                                            );
                                                            nbformat_value.to_string()
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        "[kernel-manager] Failed to create page manifest: {}",
                                                        e
                                                    );
                                                    nbformat_value.to_string()
                                                }
                                            };

                                            // Append to Automerge doc
                                            let persist_bytes = {
                                                let mut doc_guard = shell_doc.write().await;
                                                if let Err(e) =
                                                    doc_guard.append_output(cid, &output_ref)
                                                {
                                                    warn!(
                                                        "[kernel-manager] Failed to append page output to doc: {}",
                                                        e
                                                    );
                                                }
                                                let bytes = doc_guard.save();
                                                let _ = shell_changed_tx.send(());
                                                bytes
                                            };
                                            persist_notebook_bytes(
                                                &persist_bytes,
                                                &shell_persist_path,
                                            );

                                            // Broadcast to all windows
                                            let _ = shell_broadcast_tx.send(
                                                NotebookBroadcast::Output {
                                                    cell_id: cid.clone(),
                                                    output_type: "display_data".to_string(),
                                                    output_json: output_ref,
                                                },
                                            );
                                        }
                                    }
                                }

                                // Broadcast execution done for error status
                                if reply.status != jupyter_protocol::ReplyStatus::Ok {
                                    if let Some(ref cid) = cell_id {
                                        let _ = shell_broadcast_tx.send(
                                            NotebookBroadcast::ExecutionDone {
                                                cell_id: cid.clone(),
                                            },
                                        );
                                    }
                                }

                                // Note: cell_id_map cleanup happens on cell re-execution, not here.
                                // Both shell and iopub channels need the mapping, and they race.
                            }
                            JupyterMessageContent::HistoryReply(ref reply) => {
                                // Get the parent msg_id to find the pending request
                                if let Some(ref parent) = msg.parent_header {
                                    let msg_id = &parent.msg_id;
                                    if let Ok(mut pending) = shell_pending_history.lock() {
                                        if let Some(tx) = pending.remove(msg_id) {
                                            // Convert Jupyter history to our format
                                            let entries: Vec<HistoryEntry> = reply
                                                .history
                                                .iter()
                                                .map(|item| {
                                                    // History items are (session, line, input)
                                                    // where input can be String or (String, String) for input/output
                                                    match item {
                                                        jupyter_protocol::HistoryEntry::Input(
                                                            session,
                                                            line,
                                                            source,
                                                        ) => HistoryEntry {
                                                            session: *session as i32,
                                                            line: *line as i32,
                                                            source: source.clone(),
                                                        },
                                                        jupyter_protocol::HistoryEntry::InputOutput(
                                                            session,
                                                            line,
                                                            (source, _output),
                                                        ) => HistoryEntry {
                                                            session: *session as i32,
                                                            line: *line as i32,
                                                            source: source.clone(),
                                                        },
                                                    }
                                                })
                                                .collect();

                                            debug!(
                                                "[kernel-manager] Resolved history request: {} entries",
                                                entries.len()
                                            );
                                            let _ = tx.send(entries);
                                        }
                                    }
                                }
                            }
                            _ => {
                                debug!(
                                    "[kernel-manager] shell reply: type={}",
                                    msg.header.msg_type
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!("[kernel-manager] shell read error: {}", e);
                        break;
                    }
                }
            }
        });

        // Store command receiver for sync server to poll
        // (the sync server will call execution_done when it receives ExecutionDone)
        self.cmd_rx = Some(cmd_rx);

        // Store state
        self.connection_info = Some(connection_info);
        self.connection_file = Some(connection_file_path);
        self.iopub_task = Some(iopub_task);
        self.shell_reader_task = Some(shell_reader_task);
        self.shell_writer = Some(shell_writer);
        self.process = Some(process);
        self.status = KernelStatus::Idle;

        // Broadcast idle status
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "idle".to_string(),
            cell_id: None,
        });

        info!("[kernel-manager] Kernel started: {}", kernel_id);
        Ok(())
    }

    /// Queue a cell for execution.
    ///
    /// Idempotent: if the cell is already executing or queued, this is a no-op.
    /// This prevents duplicate executions when multiple windows trigger RunAllCells.
    pub async fn queue_cell(&mut self, cell_id: String, code: String) -> Result<()> {
        // Skip if already executing or queued (idempotent)
        if self.executing.as_ref() == Some(&cell_id) {
            info!(
                "[kernel-manager] Cell {} already executing, skipping",
                cell_id
            );
            return Ok(());
        }
        if self.queue.iter().any(|c| c.cell_id == cell_id) {
            info!("[kernel-manager] Cell {} already queued, skipping", cell_id);
            return Ok(());
        }

        info!("[kernel-manager] Queuing cell: {}", cell_id);

        // Add to queue
        self.queue.push_back(QueuedCell {
            cell_id: cell_id.clone(),
            code,
        });

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing.clone(),
            queued: self.queued_cells(),
        });

        // Try to process if nothing executing
        self.process_next().await
    }

    /// Clear outputs for a cell (before re-execution).
    pub fn clear_outputs(&self, cell_id: &str) {
        info!("[kernel-manager] Clearing outputs for cell: {}", cell_id);
        // The frontend handles the actual clearing; we just broadcast the event
        // so all windows know to clear
    }

    /// Process the next cell in the queue.
    async fn process_next(&mut self) -> Result<()> {
        // Already executing?
        if self.executing.is_some() {
            return Ok(());
        }

        // Get next cell
        let Some(cell) = self.queue.pop_front() else {
            return Ok(());
        };

        // Check kernel is running
        if self.shell_writer.is_none() {
            return Err(anyhow::anyhow!("No kernel running"));
        }

        self.executing = Some(cell.cell_id.clone());
        self.status = KernelStatus::Busy;

        // Collect queue state before borrowing shell_writer
        let executing = self.executing.clone();
        let queued = self.queued_cells();

        // Broadcast queue state
        let _ = self
            .broadcast_tx
            .send(NotebookBroadcast::QueueChanged { executing, queued });

        // Send execute request
        let request = ExecuteRequest::new(cell.code.clone());
        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        // Register msg_id → cell_id BEFORE sending.
        // First, remove any old mappings for this cell_id (from previous executions).
        // This bounds the map to one entry per cell, not per execution, while still
        // allowing both shell (execute_reply) and iopub (idle status) to use the mapping.
        {
            let mut map = self.cell_id_map.lock().unwrap();
            map.retain(|_, v| v != &cell.cell_id);
            map.insert(msg_id.clone(), cell.cell_id.clone());
        }

        // Now borrow shell_writer mutably
        let shell = self.shell_writer.as_mut().unwrap();
        shell.send(message).await?;
        info!(
            "[kernel-manager] Sent execute_request: msg_id={} cell_id={}",
            msg_id, cell.cell_id
        );

        Ok(())
    }

    /// Mark a cell execution as complete and process next.
    pub async fn execution_done(&mut self, cell_id: &str) -> Result<()> {
        if self.executing.as_ref() == Some(&cell_id.to_string()) {
            self.executing = None;
            self.status = KernelStatus::Idle;

            // Note: cell_id_map cleanup happens when a cell is RE-EXECUTED (in
            // send_execute_request), not here. The shell and iopub channels race,
            // and both need the mapping. Cleaning up on re-execution bounds the map
            // to one entry per cell while avoiding the race condition.

            // Broadcast done
            let _ = self.broadcast_tx.send(NotebookBroadcast::ExecutionDone {
                cell_id: cell_id.to_string(),
            });

            // Broadcast queue state
            let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
                executing: None,
                queued: self.queued_cells(),
            });

            // Process next
            self.process_next().await?;
        }
        Ok(())
    }

    /// Interrupt the currently executing cell.
    pub async fn interrupt(&self) -> Result<()> {
        let connection_info = self
            .connection_info
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let mut control =
            runtimelib::create_client_control_connection(connection_info, &self.session_id).await?;

        let request: JupyterMessage = InterruptRequest {}.into();
        control.send(request).await?;

        info!("[kernel-manager] Sent interrupt_request");
        Ok(())
    }

    /// Send a comm message to the kernel (for widget interactions).
    ///
    /// Accepts the full Jupyter message envelope from the frontend to preserve
    /// header/session for proper widget protocol compliance.
    pub async fn send_comm_message(&mut self, raw_message: serde_json::Value) -> Result<()> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        // Parse header from the raw message
        let header: jupyter_protocol::Header = serde_json::from_value(
            raw_message
                .get("header")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing header in comm message"))?,
        )?;

        let msg_type = header.msg_type.clone();

        // Parse parent_header (may be null or missing)
        let parent_header: Option<jupyter_protocol::Header> =
            raw_message.get("parent_header").and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    serde_json::from_value(v.clone()).ok()
                }
            });

        // Parse metadata (defaults to empty object)
        let metadata: serde_json::Value = raw_message
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Parse content and convert to JupyterMessageContent
        let content_value = raw_message
            .get("content")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Missing content in comm message"))?;

        let message_content =
            JupyterMessageContent::from_type_and_content(&msg_type, content_value)?;

        // Parse buffers from Vec<Vec<u8>> (JSON number arrays)
        let buffers: Vec<Bytes> = raw_message
            .get("buffers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|buf| {
                        buf.as_array().map(|bytes| {
                            let bytes: Vec<u8> = bytes
                                .iter()
                                .filter_map(|b| b.as_u64().map(|n| n as u8))
                                .collect();
                            Bytes::from(bytes)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Construct the JupyterMessage with the frontend's original header
        let message = JupyterMessage {
            zmq_identities: Vec::new(),
            header,
            parent_header,
            metadata,
            content: message_content,
            buffers,
            channel: Some(jupyter_protocol::Channel::Shell),
        };

        debug!(
            "[kernel-manager] Sending comm message: type={} msg_id={}",
            msg_type, message.header.msg_id
        );

        shell.send(message).await?;
        Ok(())
    }

    /// Search kernel input history.
    ///
    /// Sends a history_request to the kernel and waits for the reply.
    /// Returns an error if no kernel is running or the request times out.
    pub async fn get_history(
        &mut self,
        pattern: Option<String>,
        n: i32,
        unique: bool,
    ) -> Result<Vec<HistoryEntry>> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        // Create history request
        let request = HistoryRequest::Search {
            pattern: pattern.unwrap_or_else(|| "*".to_string()),
            unique,
            output: false,
            raw: true,
            n,
        };

        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        // Create response channel
        let (tx, rx) = oneshot::channel();

        // Register pending request BEFORE sending
        self.pending_history
            .lock()
            .map_err(|_| anyhow::anyhow!("Lock poisoned"))?
            .insert(msg_id.clone(), tx);

        // Send request
        shell.send(message).await?;
        debug!("[kernel-manager] Sent history_request: msg_id={}", msg_id);

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(entries)) => Ok(entries),
            Ok(Err(_)) => {
                // Channel closed without response
                Err(anyhow::anyhow!("History request cancelled"))
            }
            Err(_) => {
                // Timeout - clean up pending request
                if let Ok(mut pending) = self.pending_history.lock() {
                    pending.remove(&msg_id);
                }
                Err(anyhow::anyhow!("History request timed out"))
            }
        }
    }

    /// Clear the execution queue.
    pub fn clear_queue(&mut self) -> Vec<String> {
        let cleared: Vec<String> = self.queue.drain(..).map(|c| c.cell_id).collect();

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing.clone(),
            queued: vec![],
        });

        cleared
    }

    /// Shutdown the kernel.
    pub async fn shutdown(&mut self) -> Result<()> {
        info!("[kernel-manager] Shutting down kernel");

        self.status = KernelStatus::ShuttingDown;

        // Broadcast shutdown status
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "shutdown".to_string(),
            cell_id: None,
        });

        // Abort tasks
        if let Some(task) = self.iopub_task.take() {
            task.abort();
        }
        if let Some(task) = self.shell_reader_task.take() {
            task.abort();
        }

        // Try graceful shutdown via shell
        if let Some(mut shell) = self.shell_writer.take() {
            let request: JupyterMessage = ShutdownRequest { restart: false }.into();
            let _ = shell.send(request).await;
        }

        // Kill process group on Unix
        #[cfg(unix)]
        if let Some(pgid) = self.process_group_id.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            if let Err(e) = killpg(Pid::from_raw(pgid), Signal::SIGKILL) {
                if e != nix::errno::Errno::ESRCH {
                    error!(
                        "[kernel-manager] Failed to kill process group {}: {}",
                        pgid, e
                    );
                }
            }
        }

        // Clean up process
        self.process = None;

        // Clean up connection file
        if let Some(ref path) = self.connection_file {
            let _ = std::fs::remove_file(path);
        }

        // Clear state
        self.connection_info = None;
        self.connection_file = None;
        self.cell_id_map.lock().unwrap().clear();
        self.queue.clear();
        self.executing = None;
        self.cmd_tx = None;

        info!("[kernel-manager] Kernel shutdown complete");
        Ok(())
    }
}

impl Drop for RoomKernel {
    fn drop(&mut self) {
        // Abort any running tasks
        if let Some(task) = self.iopub_task.take() {
            task.abort();
        }
        if let Some(task) = self.shell_reader_task.take() {
            task.abort();
        }

        // Kill process group on Unix
        #[cfg(unix)]
        if let Some(pgid) = self.process_group_id.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }

        // Clean up connection file
        if let Some(ref path) = self.connection_file {
            let _ = std::fs::remove_file(path);
        }

        info!("[kernel-manager] RoomKernel dropped - resources cleaned up");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_status_display() {
        assert_eq!(KernelStatus::Starting.to_string(), "starting");
        assert_eq!(KernelStatus::Idle.to_string(), "idle");
        assert_eq!(KernelStatus::Busy.to_string(), "busy");
        assert_eq!(KernelStatus::Error.to_string(), "error");
        assert_eq!(KernelStatus::ShuttingDown.to_string(), "shutdown");
    }

    #[test]
    fn test_kernel_status_serialize() {
        let json = serde_json::to_string(&KernelStatus::Idle).unwrap();
        assert_eq!(json, "\"idle\"");
    }

    #[test]
    fn test_room_kernel_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, _rx) = broadcast::channel(16);
        let (changed_tx, _changed_rx) = broadcast::channel(16);
        let doc = Arc::new(RwLock::new(NotebookDoc::new("test-notebook")));
        let persist_path = PathBuf::from("/tmp/test.automerge");
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));
        let comm_state = Arc::new(CommState::new());
        let kernel = RoomKernel::new(tx, doc, persist_path, changed_tx, blob_store, comm_state);

        assert!(!kernel.is_running());
        assert!(kernel.executing_cell().is_none());
        assert!(kernel.queued_cells().is_empty());
        assert_eq!(kernel.status(), KernelStatus::Starting);
    }
}
