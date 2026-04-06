// Mutex::lock only fails if another thread panicked while holding the lock.
// In that case the program is already crashing, so unwrap is acceptable here.
// The shell_writer unwrap at line 1713 is guarded by an early-return check at line 1681.
#![allow(clippy::unwrap_used)]

//! Kernel management for runtime agent subprocesses.
//!
//! Each agent subprocess owns one kernel. The agent manages the kernel lifecycle
//! and execution queue, writing outputs to RuntimeStateDoc which syncs to all
//! connected peers via Automerge.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use bytes::Bytes;
use jupyter_protocol::{
    CompleteRequest, ConnectionInfo, ExecuteRequest, HistoryRequest, InterruptRequest,
    JupyterMessage, JupyterMessageContent, KernelInfoRequest, ShutdownRequest,
};
use log::{debug, error, info, warn};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot, watch, RwLock};
use uuid::Uuid;

use crate::blob_store::BlobStore;
use crate::notebook_doc::NotebookDoc;
use crate::output_store::{self, ContentRef, OutputManifest, DEFAULT_INLINE_THRESHOLD};
use crate::protocol::{CompletionItem, HistoryEntry, NotebookBroadcast, QueueEntry};
use crate::stream_terminal::{StreamOutputState, StreamTerminals};
use crate::terminal_size::{TERMINAL_COLUMNS_STR, TERMINAL_LINES_STR};
use crate::{EnvType, PooledEnv};
use notebook_doc::presence::{self, PresenceState};
use notebook_doc::runtime_state::{QueueEntry as DocQueueEntry, RuntimeStateDoc};

/// Maximum execution entries retained in the RuntimeStateDoc.
/// `trim_executions` preserves the latest execution per cell, so the actual
/// count can exceed this if the notebook has more unique cells than this limit.
/// Keep this low — each entry adds O(1) to every output diff in the WASM sync
/// path, and rapid re-execution of the same cell accumulates entries fast.
const MAX_EXECUTION_ENTRIES: usize = 64;

/// Store widget buffers in the blob store and replace values in the state
/// dict with `{"$blob": "<hash>"}` sentinels at the given buffer_paths.
///
/// Returns the modified state and the buffer_paths used. If there are no
/// buffers or no buffer_paths, returns the state unchanged and empty paths.
async fn store_widget_buffers(
    state: &serde_json::Value,
    buffer_paths: &[Vec<String>],
    buffers: &[Vec<u8>],
    blob_store: &crate::blob_store::BlobStore,
) -> (serde_json::Value, Vec<Vec<String>>) {
    if buffers.is_empty() || buffer_paths.is_empty() {
        return (state.clone(), vec![]);
    }

    let mut modified = state.clone();
    let mut used_paths = Vec::new();

    for (i, path) in buffer_paths.iter().enumerate() {
        if i >= buffers.len() || path.is_empty() {
            continue;
        }

        // Store buffer in blob store
        let hash = match blob_store
            .put(&buffers[i], "application/octet-stream")
            .await
        {
            Ok(h) => h,
            Err(e) => {
                log::warn!("[kernel-manager] Failed to store widget buffer: {}", e);
                continue;
            }
        };

        // Navigate to the parent in the state dict and replace with sentinel.
        // Handles both object keys (strings) and array indices (integers).
        if let Some(last_key) = path.last() {
            let parent_path = &path[..path.len() - 1];
            let parent = parent_path
                .iter()
                .try_fold(&mut modified, |v, key| json_get_mut(v, key));
            if let Some(parent) = parent {
                let sentinel = serde_json::json!({"$blob": hash});
                if let Some(obj) = parent.as_object_mut() {
                    obj.insert(last_key.clone(), sentinel);
                    used_paths.push(path.clone());
                } else if let Some(arr) = parent.as_array_mut() {
                    if let Ok(idx) = last_key.parse::<usize>() {
                        if idx < arr.len() {
                            arr[idx] = sentinel;
                            used_paths.push(path.clone());
                        }
                    }
                }
            }
        }
    }

    (modified, used_paths)
}

/// Navigate into a JSON value by key (object) or index (array).
fn json_get_mut<'a>(v: &'a mut serde_json::Value, key: &str) -> Option<&'a mut serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map.get_mut(key),
        serde_json::Value::Array(arr) => key.parse::<usize>().ok().and_then(|idx| arr.get_mut(idx)),
        _ => None,
    }
}

/// Extract buffer_paths from a Jupyter comm data payload.
fn extract_buffer_paths(data: &serde_json::Value) -> Vec<Vec<String>> {
    data.get("buffer_paths")
        .or_else(|| data.get("state").and_then(|s| s.get("buffer_paths")))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|path| {
                    path.as_array().map(|p| {
                        p.iter()
                            .filter_map(|s| {
                                // Handle both string and integer path segments
                                // (ipywidgets uses integers for list indices)
                                s.as_str()
                                    .map(|v| v.to_string())
                                    .or_else(|| s.as_u64().map(|v| v.to_string()))
                                    .or_else(|| s.as_i64().map(|v| v.to_string()))
                            })
                            .collect()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Threshold (bytes) for blob-storing comm state values.
/// Properties whose JSON serialization exceeds this size are replaced with
/// `{"$blob": "<hash>"}` sentinels. This prevents catastrophically slow
/// Automerge writes for large state (e.g., anywidget `_esm` JS bundles,
/// Vega-Lite `spec` dicts with embedded datasets).
const COMM_STATE_BLOB_THRESHOLD: usize = 1024;

/// Scan the top-level properties of a comm state object and replace any
/// whose JSON-serialized size exceeds `COMM_STATE_BLOB_THRESHOLD` with
/// `{"$blob": "<hash>"}` sentinels stored in the blob store.
///
/// Only top-level keys are checked — we don't recurse into nested objects.
/// This is intentional: the Automerge cost comes from expanding large nested
/// structures into per-key CRDT entries, and the top-level is where `spec`,
/// `_esm`, and `_css` live.
///
/// Returns the modified state with large values replaced by sentinels.
async fn blob_store_large_state_values(
    state: &serde_json::Value,
    blob_store: &BlobStore,
) -> serde_json::Value {
    let Some(obj) = state.as_object() else {
        return state.clone();
    };

    let mut modified = serde_json::Map::with_capacity(obj.len());

    for (key, value) in obj {
        // Serialize to check size. For strings we can check len() directly;
        // for objects/arrays we need the JSON representation.
        let size = match value {
            serde_json::Value::String(s) => s.len(),
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
            }
            // Scalars (bool, number, null) are always small.
            _ => 0,
        };

        if size > COMM_STATE_BLOB_THRESHOLD {
            // Serialize and store in blob store.
            // Use the raw string bytes for strings (preserves content type for
            // _esm JavaScript, _css stylesheets, etc.) and JSON for structured values.
            let (blob_bytes, media_type) = match value {
                serde_json::Value::String(s) => {
                    let mime = match key.as_str() {
                        "_esm" => "text/javascript",
                        "_css" => "text/css",
                        _ => "text/plain",
                    };
                    (s.as_bytes().to_vec(), mime)
                }
                _ => match serde_json::to_vec(value) {
                    Ok(b) => (b, "application/json"),
                    Err(e) => {
                        log::warn!(
                            "[kernel-manager] Failed to serialize comm state key '{}': {}",
                            key,
                            e
                        );
                        modified.insert(key.clone(), value.clone());
                        continue;
                    }
                },
            };
            match blob_store.put(&blob_bytes, media_type).await {
                Ok(hash) => {
                    modified.insert(key.clone(), serde_json::json!({"$blob": hash}));
                }
                Err(e) => {
                    log::warn!(
                        "[kernel-manager] Failed to blob-store comm state key '{}' ({} bytes): {}",
                        key,
                        size,
                        e
                    );
                    modified.insert(key.clone(), value.clone());
                }
            }
        } else {
            modified.insert(key.clone(), value.clone());
        }
    }

    serde_json::Value::Object(modified)
}

/// Convert a protocol QueueEntry to a RuntimeStateDoc QueueEntry.
fn to_doc_entry(e: &QueueEntry) -> DocQueueEntry {
    DocQueueEntry {
        cell_id: e.cell_id.clone(),
        execution_id: e.execution_id.clone(),
    }
}

/// Convert a slice of protocol QueueEntries to doc QueueEntries.
fn to_doc_entries(entries: &[QueueEntry]) -> Vec<DocQueueEntry> {
    entries.iter().map(to_doc_entry).collect()
}

// ── Launched Environment Config ─────────────────────────────────────────────

/// Re-export environment config types from the shared protocol crate.
/// The canonical definitions live in `notebook_protocol::protocol`.
pub use notebook_protocol::protocol::{DenoLaunchedConfig, LaunchedEnvConfig};

// ── Output Conversion ───────────────────────────────────────────────────────

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

/// Update an output by display_id when outputs are inline manifests.
///
/// Iterates through all executions' outputs in the RuntimeStateDoc,
/// deserializes each as an OutputManifest, checks for a matching display_id,
/// and replaces the manifest with updated data when found.
///
/// Returns true if an output was found and updated, false otherwise.
async fn update_output_by_display_id_with_manifests(
    state_doc: &mut RuntimeStateDoc,
    display_id: &str,
    new_data: &serde_json::Value,
    new_metadata: &serde_json::Map<String, serde_json::Value>,
    blob_store: &BlobStore,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Get all outputs from the RuntimeStateDoc (keyed by execution_id)
    let outputs = state_doc.get_all_outputs();
    let mut found = false;

    for (exec_id, output_idx, output_value) in outputs {
        // Deserialize the inline manifest JSON Value as an OutputManifest
        let manifest: OutputManifest = match serde_json::from_value(output_value) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Check if this manifest has the matching display_id and update it
        if let Some(updated_manifest) = output_store::update_manifest_display_data(
            &manifest,
            display_id,
            new_data,
            new_metadata,
            blob_store,
            DEFAULT_INLINE_THRESHOLD,
        )
        .await?
        {
            // Write the updated manifest back as a JSON Value
            state_doc.replace_output(&exec_id, output_idx, &updated_manifest.to_json())?;
            found = true;
        }
    }

    Ok(found)
}

/// A cell queued for execution.
#[derive(Debug, Clone)]
pub struct QueuedCell {
    pub cell_id: String,
    pub execution_id: String,
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
    /// Kernel process died unexpectedly
    Dead,
}

impl std::fmt::Display for KernelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelStatus::Starting => write!(f, "starting"),
            KernelStatus::Idle => write!(f, "idle"),
            KernelStatus::Busy => write!(f, "busy"),
            KernelStatus::Error => write!(f, "error"),
            KernelStatus::ShuttingDown => write!(f, "shutdown"),
            // Dead maps to "error" for frontend compatibility (frontend only recognizes
            // not_started, starting, idle, busy, error, shutdown)
            KernelStatus::Dead => write!(f, "error"),
        }
    }
}

/// A kernel owned by the daemon for a notebook room.
///
/// Type alias for pending completion response channels.
type PendingCompletions =
    Arc<StdMutex<HashMap<String, oneshot::Sender<(Vec<CompletionItem>, usize, usize)>>>>;

/// Unlike the notebook app's `NotebookKernel`, this broadcasts outputs
/// to all connected peers rather than emitting Tauri events.
pub struct RoomKernel {
    /// Kernel type (e.g., "python", "deno")
    kernel_type: String,
    /// Environment source (e.g., "uv:inline", "conda:prewarmed")
    env_source: String,
    /// Environment configuration used at launch (for sync detection)
    launched_config: LaunchedEnvConfig,
    /// Path to the environment directory backing this kernel (if any).
    /// Used by the GC to avoid evicting envs that are still in use.
    pub env_path: Option<PathBuf>,
    /// Connection info for the kernel
    connection_info: Option<ConnectionInfo>,
    /// Path to the connection file
    connection_file: Option<PathBuf>,
    /// Session ID for Jupyter protocol
    session_id: String,
    /// Automerge actor ID for kernel writes (e.g. "rt:kernel:a1b2c3d4").
    /// Set on RuntimeStateDoc forks before mutating so output writes carry kernel provenance.
    kernel_actor_id: String,
    /// Handle to the iopub listener task
    iopub_task: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the shell reader task
    shell_reader_task: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the process watcher task (detects process exit)
    process_watcher_task: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the heartbeat monitor task (detects unresponsive kernel)
    heartbeat_task: Option<tokio::task::JoinHandle<()>>,
    /// Channel for coalesced comm state writes (IOPub → coalesce task).
    comm_coalesce_tx: Option<mpsc::UnboundedSender<(String, serde_json::Value)>>,
    /// Handle to the coalescing task for comm state CRDT writes.
    comm_coalesce_task: Option<tokio::task::JoinHandle<()>>,
    /// Shell writer for sending execute requests
    shell_writer: Option<runtimelib::DealerSendConnection>,
    /// Process group ID for cleanup (Unix only)
    #[cfg(unix)]
    process_group_id: Option<i32>,
    /// Kernel ID for the process registry (orphan reaping)
    kernel_id: Option<String>,
    /// Mapping from msg_id → (cell_id, execution_id) for routing iopub messages
    cell_id_map: Arc<StdMutex<HashMap<String, (String, String)>>>,
    /// Execution queue (pending cells)
    queue: VecDeque<QueuedCell>,
    /// Currently executing cell: (cell_id, execution_id)
    executing: Option<(String, String)>,
    /// Whether the current execution has seen an error output.
    /// Set by the iopub error handler, read by `execution_done()` to
    /// determine success/failure for the RuntimeStateDoc lifecycle entry.
    execution_had_error: bool,
    /// Current kernel status
    status: KernelStatus,
    /// Broadcast channel for sending outputs to peers
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Command sender for iopub/shell tasks
    cmd_tx: Option<mpsc::Sender<QueueCommand>>,
    /// Command receiver for queue state updates (polled by sync server)
    cmd_rx: Option<mpsc::Receiver<QueueCommand>>,
    /// Blob store for output manifests
    blob_store: Arc<BlobStore>,
    /// Monotonic counter for comm insertion order (written to RuntimeStateDoc).
    comm_seq: Arc<AtomicU64>,
    /// Pending history requests: msg_id → response channel
    pending_history: Arc<StdMutex<HashMap<String, oneshot::Sender<Vec<HistoryEntry>>>>>,
    /// Pending completion requests: msg_id → response channel
    pending_completions: PendingCompletions,
    /// Terminal emulators for stream outputs (stdout/stderr)
    stream_terminals: Arc<tokio::sync::Mutex<StreamTerminals>>,
    /// Per-notebook runtime state document (daemon-authoritative).
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    /// Notification channel for RuntimeStateDoc changes.
    state_changed_tx: broadcast::Sender<()>,
    /// Transient peer state (cursors, selections, kernel status).
    presence: Arc<RwLock<PresenceState>>,
    /// Broadcast channel for presence frames (cursor, selection, kernel state).
    presence_tx: broadcast::Sender<(String, Vec<u8>)>,
}

/// Commands from iopub/shell handlers for queue state management.
///
/// These are sent from spawned tasks and must be processed by code
/// that has access to `&mut RoomKernel` (e.g., the notebook sync server).
#[derive(Debug)]
pub enum QueueCommand {
    /// A cell finished executing (received status=idle from kernel)
    ExecutionDone {
        cell_id: String,
        execution_id: String,
    },
    /// A cell produced an error (for stop-on-error behavior)
    CellError {
        cell_id: String,
        execution_id: String,
    },
    /// The kernel process died (iopub connection lost).
    /// Unblocks the execution queue and notifies the frontend.
    KernelDied,
    /// Send a comm_msg(update) to the kernel via the shell channel.
    /// Used by the IOPub task to sync Output widget captured outputs back.
    SendCommUpdate {
        comm_id: String,
        state: serde_json::Value,
    },
}

/// Prepend a directory to the PATH environment variable.
fn prepend_to_path(dir: &std::path::Path) -> String {
    let dir_str = dir.to_string_lossy();
    match std::env::var("PATH") {
        Ok(existing) => format!("{}:{}", dir_str, existing),
        Err(_) => dir_str.to_string(),
    }
}

/// Escape a search pattern for IPython's fnmatch-based history search.
///
/// IPython's history search uses fnmatch (glob) matching, so we need to:
/// 1. Escape any glob metacharacters in the user's search term
/// 2. Wrap with *...* for substring matching
///
/// Without this, a search for "for" would only match entries exactly equal
/// to "for", not entries containing "for".
fn escape_glob_pattern(pattern: Option<&str>) -> String {
    match pattern {
        Some(p) if !p.is_empty() => {
            let mut escaped = String::with_capacity(p.len() + 2);
            escaped.push('*');
            for c in p.chars() {
                match c {
                    '*' | '?' | '[' | ']' => {
                        escaped.push('[');
                        escaped.push(c);
                        escaped.push(']');
                    }
                    _ => escaped.push(c),
                }
            }
            escaped.push('*');
            escaped
        }
        _ => "*".to_string(),
    }
}

impl RoomKernel {
    /// Create a new room kernel with a broadcast channel for outputs.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
        blob_store: Arc<BlobStore>,
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        state_changed_tx: broadcast::Sender<()>,
        presence: Arc<RwLock<PresenceState>>,
        presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let kernel_actor_id = format!("rt:kernel:{}", &session_id[..8]);
        Self {
            execution_had_error: false,
            kernel_type: String::new(),
            env_source: String::new(),
            launched_config: LaunchedEnvConfig::default(),
            connection_info: None,
            connection_file: None,
            session_id,
            kernel_actor_id,
            iopub_task: None,
            shell_reader_task: None,
            process_watcher_task: None,
            heartbeat_task: None,
            comm_coalesce_tx: None,
            comm_coalesce_task: None,
            shell_writer: None,
            #[cfg(unix)]
            process_group_id: None,
            kernel_id: None,
            cell_id_map: Arc::new(StdMutex::new(HashMap::new())),
            queue: VecDeque::new(),
            executing: None,
            status: KernelStatus::Starting,
            broadcast_tx,
            cmd_tx: None,
            cmd_rx: None,
            blob_store,
            comm_seq: Arc::new(AtomicU64::new(0)),
            pending_history: Arc::new(StdMutex::new(HashMap::new())),
            pending_completions: Arc::new(StdMutex::new(HashMap::new())),
            stream_terminals: Arc::new(tokio::sync::Mutex::new(StreamTerminals::new())),
            env_path: None,
            state_doc,
            state_changed_tx,
            presence,
            presence_tx,
        }
    }

    /// Spawn a background task that processes `QueueCommand`s from the kernel's
    /// iopub/shell/heartbeat tasks. Handles `ExecutionDone`, `CellError`, and
    /// `KernelDied` for the lifetime of the kernel.
    ///
    /// Must be called after `launch()`. The `kernel_arc` is needed because the
    /// spawned task locks it to call `execution_done()` and friends.
    pub fn start_command_loop(
        &mut self,
        kernel_arc: Arc<tokio::sync::Mutex<Option<RoomKernel>>>,
        _doc: Arc<RwLock<NotebookDoc>>,
        _persist_tx: watch::Sender<Option<Vec<u8>>>,
        _changed_tx: broadcast::Sender<()>,
    ) {
        let Some(mut cmd_rx) = self.cmd_rx.take() else {
            return;
        };

        let room_broadcast_tx = self.broadcast_tx.clone();
        let room_presence = self.presence.clone();
        let room_presence_tx = self.presence_tx.clone();
        let room_state_doc = self.state_doc.clone();
        let room_state_changed_tx = self.state_changed_tx.clone();

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    QueueCommand::ExecutionDone {
                        cell_id,
                        execution_id,
                    } => {
                        debug!(
                            "[notebook-sync] ExecutionDone for {} ({})",
                            cell_id, execution_id
                        );
                        let env_source = {
                            let mut guard = kernel_arc.lock().await;
                            if let Some(ref mut k) = *guard {
                                if let Err(e) = k.execution_done(&cell_id, &execution_id).await {
                                    warn!("[notebook-sync] execution_done error: {}", e);
                                }
                                Some(k.env_source().to_string())
                            } else {
                                None
                            }
                        };
                        if let Some(es) = env_source {
                            crate::notebook_sync_server::update_kernel_presence(
                                &room_presence,
                                &room_presence_tx,
                                presence::KernelStatus::Idle,
                                &es,
                            )
                            .await;
                        }
                    }
                    QueueCommand::CellError {
                        cell_id,
                        execution_id,
                    } => {
                        debug!(
                            "[notebook-sync] User code error, halting queue (stop-on-error): cell={} execution={}",
                            cell_id, execution_id
                        );
                        crate::notebook_sync_server::apply_cell_error_to_state_doc(
                            &kernel_arc,
                            &room_state_doc,
                            &room_state_changed_tx,
                        )
                        .await;
                    }
                    QueueCommand::SendCommUpdate { comm_id, state } => {
                        let mut guard = kernel_arc.lock().await;
                        if let Some(ref mut k) = *guard {
                            if let Err(e) = k.send_comm_update(&comm_id, state).await {
                                warn!(
                                    "[notebook-sync] Failed to send comm update to kernel: {}",
                                    e
                                );
                            }
                        }
                    }
                    QueueCommand::KernelDied => {
                        warn!("[notebook-sync] Kernel died, unblocking execution queue");
                        let env_source =
                            crate::notebook_sync_server::apply_kernel_died_to_state_doc(
                                &kernel_arc,
                                &room_state_doc,
                                &room_state_changed_tx,
                                &room_broadcast_tx,
                            )
                            .await;
                        // CRDT comms are already cleared by apply_kernel_died_to_state_doc
                        if let Some(es) = env_source {
                            crate::notebook_sync_server::update_kernel_presence(
                                &room_presence,
                                &room_presence_tx,
                                presence::KernelStatus::Errored,
                                &es,
                            )
                            .await;
                        }
                    }
                }
            }
            info!("[notebook-sync] Command receiver closed, kernel likely shutdown");
        });
    }

    /// Take the QueueCommand receiver from the kernel.
    ///
    /// Used by the agent subprocess to handle commands in its own select loop
    /// instead of calling `start_command_loop()`. Returns `None` if already taken.
    pub fn take_cmd_rx(&mut self) -> Option<mpsc::Receiver<QueueCommand>> {
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

    /// Get the environment configuration used at launch (for sync detection).
    pub fn launched_config(&self) -> &LaunchedEnvConfig {
        &self.launched_config
    }

    /// Update the UV deps in the launched config after hot-sync.
    /// This ensures future sync checks know about the newly installed packages.
    pub fn update_launched_uv_deps(&mut self, deps: Vec<String>) {
        self.launched_config.uv_deps = Some(deps);
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
    pub fn executing_cell(&self) -> Option<&str> {
        self.executing.as_ref().map(|(cid, _)| cid.as_str())
    }

    /// Get the queued cell IDs.
    pub fn queued_cells(&self) -> Vec<QueueEntry> {
        self.queue
            .iter()
            .map(|c| QueueEntry {
                cell_id: c.cell_id.clone(),
                execution_id: c.execution_id.clone(),
            })
            .collect()
    }

    /// Get the currently executing entry as a QueueEntry.
    pub fn executing_entry(&self) -> Option<QueueEntry> {
        self.executing.as_ref().map(|(cid, eid)| QueueEntry {
            cell_id: cid.clone(),
            execution_id: eid.clone(),
        })
    }

    /// Launch a kernel for this room.
    ///
    /// If `env` is provided (prewarmed pool environment), launches using that environment's
    /// Python directly. For `uv:inline` sources, uses `uv run --with` with the provided deps.
    /// For `uv:pyproject`, uses `uv run` in the project directory.
    ///
    /// Note: `conda:inline` currently falls back to prewarmed pool (inline deps not installed).
    /// TODO: Implement on-demand conda env creation for conda:inline deps.
    ///
    /// The `launched_config` captures the environment configuration used at launch time,
    /// enabling detection of metadata drift (e.g., user added deps while kernel running).
    pub async fn launch(
        &mut self,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&std::path::Path>,
        env: Option<PooledEnv>,
        launched_config: LaunchedEnvConfig,
    ) -> Result<()> {
        // Shutdown existing kernel if any (but don't broadcast shutdown for fresh kernel)
        if self.is_running() {
            self.shutdown().await.ok();
        }

        self.kernel_type = kernel_type.to_string();
        self.env_source = env_source.to_string();
        self.launched_config = launched_config;
        self.env_path = env.as_ref().map(|e| e.venv_path.clone());
        self.status = KernelStatus::Starting;

        // Broadcast starting status
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "starting".to_string(),
            cell_id: None,
        });

        {
            let mut sd = self.state_doc.write().await;
            let mut changed = false;
            changed |= sd.set_kernel_status("starting");
            changed |= sd.set_kernel_info(&self.kernel_type, &self.kernel_type, &self.env_source);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

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

        // Write connection file to the daemon's own directory (not the shared
        // Jupyter runtime dir) so we can safely clean up orphans on restart.
        let conn_dir = crate::connections_dir();
        tokio::fs::create_dir_all(&conn_dir).await?;

        let kernel_id: String =
            petname::petname(2, "-").unwrap_or_else(|| Uuid::new_v4().to_string());
        let connection_file_path = conn_dir.join(format!("{}.json", kernel_id));

        tokio::fs::write(
            &connection_file_path,
            serde_json::to_string_pretty(&connection_info)?,
        )
        .await?;

        // Determine working directory
        let cwd = if let Some(path) = notebook_path {
            if path.is_dir() {
                // For untitled notebooks, working_dir is already a directory
                path.to_path_buf()
            } else {
                // For saved notebooks, get parent directory of the file
                path.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(std::env::temp_dir)
            }
        } else {
            // No path at all - use default notebooks directory to avoid macOS permission prompts
            // (using $HOME triggers "allow access to Music/Documents/etc" popups)
            // In dev mode, this will be {workspace}/notebooks instead of ~/notebooks
            runt_workspace::default_notebooks_dir().unwrap_or_else(|_| std::env::temp_dir())
        };

        // Build kernel command based on kernel type
        let mut cmd = match kernel_type {
            "python" => {
                // Branch on env_source for different Python environment types
                match env_source {
                    "uv:inline" => {
                        // Use prepared cached environment with inline deps
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "uv:inline requires a prepared environment (was it created?)"
                            )
                        })?;
                        info!(
                            "[kernel-manager] Starting Python kernel with cached inline env at {:?}",
                            pooled_env.python_path
                        );
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", "ipykernel_launcher", "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());

                        // Set VIRTUAL_ENV so uv knows which environment to target
                        cmd.env("VIRTUAL_ENV", &pooled_env.venv_path);

                        // Add uv to PATH for shell commands like !uv pip install
                        let uv_path = kernel_launch::tools::get_uv_path().await?;
                        if let Some(uv_dir) = uv_path.parent() {
                            cmd.env("PATH", prepend_to_path(uv_dir));
                        }

                        cmd
                    }
                    "uv:pyproject" => {
                        // Use `uv run` in the project directory with ipykernel
                        let uv_path = kernel_launch::tools::get_uv_path().await?;
                        info!(
                            "[kernel-manager] Starting Python kernel with uv run (env_source: {})",
                            env_source
                        );
                        let mut cmd = tokio::process::Command::new(&uv_path);
                        cmd.args([
                            "run",
                            "--with",
                            "ipykernel",
                            "--with",
                            "uv",
                            "python",
                            "-Xfrozen_modules=off",
                            "-m",
                            "ipykernel_launcher",
                            "-f",
                        ]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        cmd
                    }
                    "conda:inline" => {
                        // Use prepared cached conda environment with inline deps
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "conda:inline requires a prepared environment (was it created?)"
                            )
                        })?;
                        info!(
                            "[kernel-manager] Starting Python kernel with cached conda inline env at {:?}",
                            pooled_env.python_path
                        );
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", "ipykernel_launcher", "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        cmd
                    }
                    "pixi:toml" => {
                        // Use pixi shell-hook to get activation env vars, then
                        // launch Python directly (faster, cleaner signal handling).
                        // Falls back to `pixi run` if shell-hook fails.
                        let manifest_path = notebook_path.and_then(|p| {
                            crate::project_file::detect_project_file(p)
                                .filter(|d| {
                                    d.kind == crate::project_file::ProjectFileKind::PixiToml
                                })
                                .map(|d| d.path)
                        });

                        if let Some(ref manifest) = manifest_path {
                            match kernel_launch::tools::pixi_shell_hook(manifest, None).await {
                                Ok(env_vars) => {
                                    // Direct launch: use Python from the pixi prefix
                                    let python = env_vars
                                        .get("CONDA_PREFIX")
                                        .map(|p| {
                                            let prefix = std::path::PathBuf::from(p);
                                            if cfg!(windows) {
                                                prefix.join("python.exe")
                                            } else {
                                                prefix.join("bin").join("python")
                                            }
                                        })
                                        .unwrap_or_else(|| std::path::PathBuf::from("python"));
                                    info!(
                                        "[kernel-manager] Starting Python kernel via pixi shell-hook ({})",
                                        python.display()
                                    );
                                    let mut cmd = tokio::process::Command::new(&python);
                                    cmd.envs(&env_vars);
                                    cmd.args([
                                        "-Xfrozen_modules=off",
                                        "-m",
                                        "ipykernel_launcher",
                                        "-f",
                                    ]);
                                    cmd.arg(&connection_file_path);
                                    cmd.stdout(Stdio::null());
                                    cmd.stderr(Stdio::piped());
                                    cmd
                                }
                                Err(e) => {
                                    // Fallback: use pixi run (env may not be installed yet)
                                    warn!(
                                        "[kernel-manager] pixi shell-hook failed ({}), falling back to pixi run",
                                        e
                                    );
                                    let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                                    let mut cmd = tokio::process::Command::new(&pixi_path);
                                    cmd.args([
                                        "run",
                                        "python",
                                        "-Xfrozen_modules=off",
                                        "-m",
                                        "ipykernel_launcher",
                                        "-f",
                                    ]);
                                    cmd.arg(&connection_file_path);
                                    cmd.stdout(Stdio::null());
                                    cmd.stderr(Stdio::piped());
                                    if let Some(parent) = manifest.parent() {
                                        cmd.current_dir(parent);
                                    }
                                    cmd
                                }
                            }
                        } else {
                            // No manifest found — fall back to pixi run
                            let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                            let mut cmd = tokio::process::Command::new(&pixi_path);
                            cmd.args([
                                "run",
                                "python",
                                "-Xfrozen_modules=off",
                                "-m",
                                "ipykernel_launcher",
                                "-f",
                            ]);
                            cmd.arg(&connection_file_path);
                            cmd.stdout(Stdio::null());
                            cmd.stderr(Stdio::piped());
                            if let Some(nb_path) = notebook_path {
                                if let Some(parent) = nb_path.parent() {
                                    cmd.current_dir(parent);
                                }
                            }
                            cmd
                        }
                    }
                    "pixi:inline" | "pixi:prewarmed" | "pixi:pep723" => {
                        // Use pixi exec with -w flags. For pixi:inline, includes
                        // user deps from notebook metadata. For pixi:prewarmed,
                        // just the base packages. Pixi caches environments at
                        // ~/.pixi/cache/cached-envs-v0/ so repeated launches are instant.
                        let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                        info!(
                            "[kernel-manager] Starting Python kernel with pixi exec (env_source: {})",
                            env_source
                        );
                        let mut cmd = tokio::process::Command::new(&pixi_path);
                        cmd.arg("exec");
                        // Always include ipykernel and common notebook packages
                        for pkg in ["ipykernel", "ipywidgets", "anywidget", "nbformat"] {
                            cmd.args(["-w", pkg]);
                        }
                        // Add user's inline deps (pixi:inline only; empty for pixi:prewarmed)
                        if let Some(ref deps) = self.launched_config.pixi_deps {
                            for dep in deps {
                                cmd.args(["-w", dep]);
                            }
                        }
                        // Add user's default pixi packages from settings
                        for pkg in &self.launched_config.prewarmed_packages {
                            cmd.args(["-w", pkg]);
                        }
                        cmd.args([
                            "python",
                            "-Xfrozen_modules=off",
                            "-m",
                            "ipykernel_launcher",
                            "-f",
                        ]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        cmd
                    }
                    _ => {
                        // Prewarmed - use pooled environment
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "Python kernel requires a pooled environment for env_source: {}",
                                env_source
                            )
                        })?;
                        info!(
                            "[kernel-manager] Starting Python kernel from env at {:?}",
                            pooled_env.python_path
                        );
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", "ipykernel_launcher", "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());

                        // Set VIRTUAL_ENV and add uv to PATH for UV prewarmed environments
                        if pooled_env.env_type == EnvType::Uv {
                            cmd.env("VIRTUAL_ENV", &pooled_env.venv_path);
                            let uv_path = kernel_launch::tools::get_uv_path().await?;
                            if let Some(uv_dir) = uv_path.parent() {
                                cmd.env("PATH", prepend_to_path(uv_dir));
                            }
                        }

                        cmd
                    }
                }
            }
            "deno" => {
                // Deno kernels use our bootstrapped deno binary
                let deno_path = kernel_launch::tools::get_deno_path().await?;
                info!("[kernel-manager] Starting Deno kernel with {:?}", deno_path);
                let mut cmd = tokio::process::Command::new(&deno_path);
                cmd.args(["jupyter", "--kernel", "--conn"]);
                cmd.arg(&connection_file_path);
                cmd.stdout(Stdio::null());
                cmd.stderr(Stdio::piped());
                cmd
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unsupported kernel type: {}. Supported types: python, deno",
                    kernel_type
                ));
            }
        };
        cmd.current_dir(&cwd);

        // Set terminal size for consistent output formatting
        cmd.env("COLUMNS", TERMINAL_COLUMNS_STR);
        cmd.env("LINES", TERMINAL_LINES_STR);

        #[cfg(unix)]
        cmd.process_group(0);

        let mut process = cmd.kill_on_drop(true).spawn()?;

        // Capture kernel stderr for diagnostics — log errors/warnings at warn level,
        // everything else at debug to avoid flooding logs with uv's verbose output
        if let Some(stderr) = process.stderr.take() {
            let kid = kernel_id.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let lower = line.to_ascii_lowercase();
                    if lower.contains("error") || lower.contains("traceback") {
                        warn!("[kernel-stderr:{}] {}", kid, line);
                    } else {
                        debug!("[kernel-stderr:{}] {}", kid, line);
                    }
                }
            });
        }

        #[cfg(unix)]
        {
            self.process_group_id = process.id().map(|pid| pid as i32);
            if let Some(pgid) = self.process_group_id {
                crate::kernel_pids::register_kernel(&kernel_id, pgid);
            }
        }
        self.kernel_id = Some(kernel_id.clone());

        info!(
            "[kernel-manager] Spawned kernel process (pid={:?}, kernel_id={})",
            process.id(),
            kernel_id
        );

        // Small delay to let the kernel start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Early crash detection: check if process exited during startup
        match process.try_wait() {
            Ok(Some(exit_status)) => {
                error!(
                    "[kernel-manager] Kernel process exited immediately: {} (kernel_id={})",
                    exit_status, kernel_id
                );
                return Err(anyhow::anyhow!(
                    "Kernel process exited immediately: {}",
                    exit_status
                ));
            }
            Ok(None) => {
                // Process still running — good
            }
            Err(e) => {
                warn!(
                    "[kernel-manager] Could not check kernel process status: {}",
                    e
                );
            }
        }

        self.session_id = Uuid::new_v4().to_string();

        // Transition to "connecting" phase — process is alive, now connecting ZMQ
        {
            let mut sd = self.state_doc.write().await;
            if sd.set_starting_phase("connecting") {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Create iopub connection and spawn listener
        let mut iopub =
            runtimelib::create_client_iopub_connection(&connection_info, "", &self.session_id)
                .await?;

        // Create command channel for queue processing
        let (cmd_tx, cmd_rx) = mpsc::channel::<QueueCommand>(100);
        self.cmd_tx = Some(cmd_tx.clone());

        // Spawn process watcher - detects process exit and signals via oneshot
        let process_cmd_tx = cmd_tx.clone();
        let (died_tx, died_rx) = tokio::sync::oneshot::channel::<String>();
        let process_watcher_task = tokio::spawn(async move {
            let status = process.wait().await;
            let msg = match status {
                Ok(exit_status) => {
                    warn!("[kernel-manager] Kernel process exited: {}", exit_status);
                    format!("Kernel process exited: {}", exit_status)
                }
                Err(e) => {
                    error!("[kernel-manager] Error waiting for kernel process: {}", e);
                    format!("Error waiting for kernel process: {}", e)
                }
            };
            let _ = died_tx.send(msg);
            let _ = process_cmd_tx.try_send(QueueCommand::KernelDied);
        });
        // Store immediately so early-return error paths can clean it up
        self.process_watcher_task = Some(process_watcher_task);

        let broadcast_tx = self.broadcast_tx.clone();
        let cell_id_map = self.cell_id_map.clone();
        let iopub_cmd_tx = cmd_tx.clone();
        let blob_store = self.blob_store.clone();
        let comm_seq = self.comm_seq.clone();
        let stream_terminals = self.stream_terminals.clone();
        let state_doc_for_iopub = self.state_doc.clone();
        let state_changed_for_iopub = self.state_changed_tx.clone();
        let iopub_actor_id = self.kernel_actor_id.clone();

        // Create coalescing channel early so the IOPub task can capture the sender.
        // The coalescing task itself is spawned later (after the IOPub task).
        let (coalesce_tx, coalesce_rx) = mpsc::unbounded_channel::<(String, serde_json::Value)>();
        let comm_coalesce_tx = Some(coalesce_tx.clone());

        let iopub_task = tokio::spawn(async move {
            // Track Output widgets with pending clear_output(wait=true).
            // On the next append_comm_output for these widgets, clear first.
            let mut pending_clear_widgets: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Capture routing cache: msg_id → comm_id.
            // Output widgets set capture_msg_id in the CRDT; this cache
            // provides O(1) lookup on the hot IOPub path without async CRDT reads.
            let mut capture_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            loop {
                match iopub.read().await {
                    Ok(message) => {
                        let iopub_start = std::time::Instant::now();
                        let msg_type = message.header.msg_type.clone();
                        debug!(
                            "[iopub] type={} parent_msg_id={:?}",
                            msg_type,
                            message.parent_header.as_ref().map(|h| &h.msg_id)
                        );

                        // Look up (cell_id, execution_id) from msg_id
                        let cell_entry = message
                            .parent_header
                            .as_ref()
                            .and_then(|h| cell_id_map.lock().ok()?.get(&h.msg_id).cloned());
                        let cell_id = cell_entry.as_ref().map(|(cid, _)| cid.clone());
                        let execution_id = cell_entry.as_ref().map(|(_, eid)| eid.clone());

                        // Handle different message types
                        match &message.content {
                            JupyterMessageContent::Status(status) => {
                                let status_str = match status.execution_state {
                                    jupyter_protocol::ExecutionState::Busy => "busy",
                                    jupyter_protocol::ExecutionState::Idle => "idle",
                                    jupyter_protocol::ExecutionState::Starting => "starting",
                                    // Normalize "restarting" to "starting" — same user-facing meaning
                                    jupyter_protocol::ExecutionState::Restarting => "starting",
                                    jupyter_protocol::ExecutionState::Terminating
                                    | jupyter_protocol::ExecutionState::Dead => "shutdown",
                                    _ => "unknown",
                                };

                                // Broadcast to all peers immediately for real-time UI
                                let _ = broadcast_tx.send(NotebookBroadcast::KernelStatus {
                                    status: status_str.to_string(),
                                    cell_id: cell_id.clone(),
                                });

                                // Write to RuntimeStateDoc — but skip transient
                                // busy/idle from non-execution requests (autocomplete,
                                // inspect, etc.) which have no cell_entry. These cause
                                // rapid busy→idle thrashing that generates excessive
                                // Automerge sync traffic. The broadcast above still
                                // fires for real-time UI; only the CRDT write is skipped.
                                if status_str != "unknown" {
                                    let is_transient = cell_entry.is_none()
                                        && (status_str == "busy" || status_str == "idle");

                                    if !is_transient {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        if sd.set_kernel_status(status_str) {
                                            let _ = state_changed_for_iopub.send(());
                                        }
                                    }
                                }

                                // Signal execution done when idle
                                if status.execution_state == jupyter_protocol::ExecutionState::Idle
                                {
                                    if let Some((cid, eid)) = cell_entry.clone() {
                                        let _ =
                                            iopub_cmd_tx.try_send(QueueCommand::ExecutionDone {
                                                cell_id: cid,
                                                execution_id: eid,
                                            });
                                    }
                                }
                            }

                            JupyterMessageContent::ExecuteInput(input) => {
                                if let Some(ref cid) = cell_id {
                                    let execution_count = input.execution_count.0 as i64;

                                    // Write execution_count to RuntimeStateDoc (sole source of truth).
                                    // The save path resolves execution_count from here.
                                    if let Some(ref eid) = execution_id {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        if sd.set_execution_count(eid, execution_count) {
                                            let _ = state_changed_for_iopub.send(());
                                        }
                                    }

                                    let _ =
                                        broadcast_tx.send(NotebookBroadcast::ExecutionStarted {
                                            cell_id: cid.clone(),
                                            execution_id: execution_id.clone().unwrap_or_default(),
                                            execution_count,
                                        });
                                }
                            }

                            // Stream outputs use terminal emulation to handle escape sequences
                            // like carriage returns (for progress bars) properly
                            JupyterMessageContent::StreamContent(stream) => {
                                // Check if this output should go to an Output widget
                                let parent_msg_id = message
                                    .parent_header
                                    .as_ref()
                                    .map(|h| h.msg_id.as_str())
                                    .unwrap_or("");
                                if let Some(widget_comm_id) =
                                    capture_cache.get(parent_msg_id).cloned()
                                {
                                    // Route to Output widget via comm_msg with method="custom"
                                    let stream_name = match stream.name {
                                        jupyter_protocol::Stdio::Stdout => "stdout",
                                        jupyter_protocol::Stdio::Stderr => "stderr",
                                    };
                                    let output = serde_json::json!({
                                        "output_type": "stream",
                                        "name": stream_name,
                                        "text": stream.text
                                    });

                                    // Persist to CRDT as inline manifest
                                    if let Ok(manifest) = crate::output_store::create_manifest(
                                        &output,
                                        &blob_store,
                                        crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                    )
                                    .await
                                    {
                                        let manifest_json = manifest.to_json();
                                        let mut sd = state_doc_for_iopub.write().await;
                                        // Honor deferred clear_output(wait=true)
                                        if pending_clear_widgets.remove(&widget_comm_id) {
                                            sd.clear_comm_outputs(&widget_comm_id);
                                        }
                                        if sd.append_comm_output(&widget_comm_id, &manifest_json) {
                                            let _ = state_changed_for_iopub.send(());
                                        }

                                        // Read the full outputs list and sync to kernel
                                        if let Some(entry) = sd.get_comm(&widget_comm_id) {
                                            let output_manifests = entry.outputs.clone();

                                            // Write manifests to state.outputs so the
                                            // frontend CRDT watcher picks them up (it diffs
                                            // entry.state, not entry.outputs).
                                            let manifests_json =
                                                serde_json::Value::Array(output_manifests.clone());
                                            sd.set_comm_state_property(
                                                &widget_comm_id,
                                                "outputs",
                                                &manifests_json,
                                            );

                                            drop(sd); // Release lock before async work

                                            // Resolve inline manifests to nbformat JSON
                                            let mut resolved_outputs = Vec::new();
                                            for m in &output_manifests {
                                                if let Ok(manifest) =
                                                    serde_json::from_value::<OutputManifest>(
                                                        m.clone(),
                                                    )
                                                {
                                                    if let Ok(resolved) =
                                                        output_store::resolve_manifest(
                                                            &manifest,
                                                            &blob_store,
                                                        )
                                                        .await
                                                    {
                                                        resolved_outputs.push(resolved);
                                                    }
                                                }
                                            }

                                            // Send comm_msg(update, state: {outputs}) to kernel
                                            let _ = iopub_cmd_tx
                                                .send(QueueCommand::SendCommUpdate {
                                                    comm_id: widget_comm_id.clone(),
                                                    state: serde_json::json!({
                                                        "outputs": resolved_outputs,
                                                    }),
                                                })
                                                .await;
                                        }
                                    }

                                    // Frontend reads from CRDT state.outputs via the comms watcher.
                                    continue; // Skip normal cell output handling
                                }

                                if let Some(ref cid) = cell_id {
                                    let stream_name = match stream.name {
                                        jupyter_protocol::Stdio::Stdout => "stdout",
                                        jupyter_protocol::Stdio::Stderr => "stderr",
                                    };

                                    let eid = execution_id.clone().unwrap_or_default();

                                    // Feed text through terminal emulator and get known output state
                                    // (keyed by execution_id so each execution gets its own terminal)
                                    let (rendered_text, known_state) = {
                                        let mut terminals = stream_terminals.lock().await;
                                        let text = terminals.feed(&eid, stream_name, &stream.text);
                                        let state =
                                            terminals.get_output_state(&eid, stream_name).cloned();
                                        (text, state)
                                    };

                                    // Create nbformat JSON with rendered text
                                    let nbformat_value = serde_json::json!({
                                        "output_type": "stream",
                                        "name": stream_name,
                                        "text": rendered_text
                                    });

                                    // Create manifest for stream output
                                    let manifest = match output_store::create_manifest(
                                        &nbformat_value,
                                        &blob_store,
                                        DEFAULT_INLINE_THRESHOLD,
                                    )
                                    .await
                                    {
                                        Ok(m) => m,
                                        Err(e) => {
                                            warn!(
                                                "[kernel-manager] Failed to create stream manifest: {}",
                                                e
                                            );
                                            continue;
                                        }
                                    };
                                    let manifest_json = manifest.to_json();

                                    // Extract blob hash for stream output state caching
                                    let blob_hash =
                                        if let OutputManifest::Stream { ref text, .. } = manifest {
                                            match text {
                                                ContentRef::Blob { blob, .. } => blob.clone(),
                                                ContentRef::Inline { inline } => inline.clone(),
                                            }
                                        } else {
                                            String::new()
                                        };

                                    // Fork RuntimeStateDoc before upsert so concurrent edits
                                    // compose via CRDT merge. Outputs now live in the state doc
                                    // keyed by execution_id.
                                    let mut fork = {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        let mut f = sd.fork();
                                        f.set_actor(&iopub_actor_id);
                                        f
                                    };

                                    let upsert_result = fork.upsert_stream_output(
                                        &eid,
                                        stream_name,
                                        &manifest_json,
                                        known_state.as_ref(),
                                    );

                                    // Use the fork's upsert result for terminal state caching
                                    // and broadcast. The fork's index is where the upsert
                                    // actually wrote — using merged len()-1 would be wrong if
                                    // the updated entry isn't the last output.
                                    let merge_ok = {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        if let Err(e) =
                                            crate::notebook_sync_server::catch_automerge_panic(
                                                "iopub-stream-output-merge",
                                                || sd.merge(&mut fork),
                                            )
                                        {
                                            warn!("{}", e);
                                            sd.rebuild_from_save();
                                            false
                                        } else {
                                            true
                                        }
                                    };

                                    if merge_ok {
                                        let broadcast_output_index = match &upsert_result {
                                            Ok((updated, output_index)) => {
                                                let mut terminals = stream_terminals.lock().await;
                                                terminals.set_output_state(
                                                    &eid,
                                                    stream_name,
                                                    StreamOutputState {
                                                        index: *output_index,
                                                        blob_hash: blob_hash.clone(),
                                                    },
                                                );
                                                if *updated {
                                                    Some(*output_index)
                                                } else {
                                                    None
                                                }
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to upsert stream output: {}",
                                                    e
                                                );
                                                None
                                            }
                                        };

                                        let _ = state_changed_for_iopub.send(());
                                        let _ = broadcast_tx.send(NotebookBroadcast::Output {
                                            cell_id: cid.clone(),
                                            execution_id: eid,
                                            output_type: "stream".to_string(),
                                            output_json: manifest_json.to_string(),
                                            output_index: broadcast_output_index,
                                        });
                                    }
                                }
                            }

                            // DisplayData and ExecuteResult are appended normally.
                            JupyterMessageContent::DisplayData(_)
                            | JupyterMessageContent::ExecuteResult(_) => {
                                // Check if this output should go to an Output widget
                                let parent_msg_id = message
                                    .parent_header
                                    .as_ref()
                                    .map(|h| h.msg_id.as_str())
                                    .unwrap_or("");
                                if let Some(widget_comm_id) =
                                    capture_cache.get(parent_msg_id).cloned()
                                {
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Persist to CRDT as inline manifest
                                        if let Ok(manifest) = crate::output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            let manifest_json = manifest.to_json();
                                            let mut sd = state_doc_for_iopub.write().await;
                                            // Honor deferred clear_output(wait=true)
                                            if pending_clear_widgets.remove(&widget_comm_id) {
                                                sd.clear_comm_outputs(&widget_comm_id);
                                            }
                                            if sd
                                                .append_comm_output(&widget_comm_id, &manifest_json)
                                            {
                                                let _ = state_changed_for_iopub.send(());
                                            }

                                            // Read full outputs and sync to kernel
                                            if let Some(entry) = sd.get_comm(&widget_comm_id) {
                                                let output_manifests = entry.outputs.clone();

                                                // Write manifests to state.outputs for frontend
                                                let manifests_json = serde_json::Value::Array(
                                                    output_manifests.clone(),
                                                );
                                                sd.set_comm_state_property(
                                                    &widget_comm_id,
                                                    "outputs",
                                                    &manifests_json,
                                                );

                                                drop(sd);
                                                let mut resolved_outputs = Vec::new();
                                                for m in &output_manifests {
                                                    if let Ok(manifest) =
                                                        serde_json::from_value::<OutputManifest>(
                                                            m.clone(),
                                                        )
                                                    {
                                                        if let Ok(r) =
                                                            output_store::resolve_manifest(
                                                                &manifest,
                                                                &blob_store,
                                                            )
                                                            .await
                                                        {
                                                            resolved_outputs.push(r);
                                                        }
                                                    }
                                                }
                                                let _ = iopub_cmd_tx.send(QueueCommand::SendCommUpdate {
                                                    comm_id: widget_comm_id.clone(),
                                                    state: serde_json::json!({ "outputs": resolved_outputs }),
                                                }).await;
                                            }
                                        }
                                        // Frontend reads from CRDT state.outputs.
                                    }
                                    continue; // Skip normal cell output handling
                                }

                                if let Some(ref cid) = cell_id {
                                    let output_type = match &message.content {
                                        JupyterMessageContent::DisplayData(_) => "display_data",
                                        JupyterMessageContent::ExecuteResult(_) => "execute_result",
                                        _ => "unknown",
                                    };

                                    let eid = execution_id.clone().unwrap_or_default();

                                    // Clear stream terminal state - non-stream outputs break
                                    // the stream chain, so next stream message should start fresh
                                    {
                                        let mut terminals = stream_terminals.lock().await;
                                        terminals.clear(&eid);
                                    }

                                    // Convert to nbformat JSON for storage
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Create manifest (inlines small data, blobs large data)
                                        let manifest_json = match output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            Ok(manifest) => manifest.to_json(),
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to create manifest: {}",
                                                    e
                                                );
                                                nbformat_value.clone()
                                            }
                                        };

                                        // Fork RuntimeStateDoc before appending so concurrent
                                        // edits compose via CRDT merge. Outputs live in state
                                        // doc keyed by execution_id.
                                        let mut fork = {
                                            let mut sd = state_doc_for_iopub.write().await;
                                            let mut f = sd.fork();
                                            f.set_actor(&iopub_actor_id);
                                            f
                                        };

                                        if let Err(e) = fork.append_output(&eid, &manifest_json) {
                                            warn!(
                                                "[kernel-manager] Failed to append output to state doc: {}",
                                                e
                                            );
                                        }

                                        let merge_ok = {
                                            let mut sd = state_doc_for_iopub.write().await;
                                            if let Err(e) =
                                                crate::notebook_sync_server::catch_automerge_panic(
                                                    "iopub-output-merge",
                                                    || sd.merge(&mut fork),
                                                )
                                            {
                                                warn!("{}", e);
                                                sd.rebuild_from_save();
                                                false
                                            } else {
                                                let _ = state_changed_for_iopub.send(());
                                                true
                                            }
                                        };

                                        if merge_ok {
                                            let _ = broadcast_tx.send(NotebookBroadcast::Output {
                                                cell_id: cid.clone(),
                                                execution_id: eid,
                                                output_type: output_type.to_string(),
                                                output_json: manifest_json.to_string(),
                                                output_index: None,
                                            });
                                        }
                                    }
                                }
                            }

                            // UpdateDisplayData mutates an existing output in place (e.g., progress bars).
                            // Find the output by display_id and update it, rather than appending.
                            // Outputs now live in RuntimeStateDoc keyed by execution_id.
                            JupyterMessageContent::UpdateDisplayData(update) => {
                                if let Some(ref display_id) = update.transient.display_id {
                                    // Fork RuntimeStateDoc before async blob I/O to avoid
                                    // holding the write lock during slow operations.
                                    let mut fork = {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        let mut f = sd.fork();
                                        f.set_actor(&iopub_actor_id);
                                        f
                                    };

                                    let updated = update_output_by_display_id_with_manifests(
                                        &mut fork,
                                        display_id,
                                        &serde_json::to_value(&update.data).unwrap_or_default(),
                                        &update.metadata,
                                        &blob_store,
                                    )
                                    .await;

                                    let merge_ok = {
                                        let mut sd = state_doc_for_iopub.write().await;
                                        match updated {
                                            Ok(true) => {
                                                if let Err(e) = crate::notebook_sync_server::catch_automerge_panic(
                                                    "iopub-display-update-merge",
                                                    || sd.merge(&mut fork),
                                                ) {
                                                    warn!("{}", e);
                                                    sd.rebuild_from_save();
                                                    false
                                                } else {
                                                    debug!(
                                                        "[kernel-manager] Updated display_id={}",
                                                        display_id
                                                    );
                                                    true
                                                }
                                            }
                                            Ok(false) => {
                                                error!(
                                                    "[kernel-manager] No output found for display_id={}",
                                                    display_id
                                                );
                                                false
                                            }
                                            Err(e) => {
                                                error!(
                                                    "[kernel-manager] Failed to update display: {}",
                                                    e
                                                );
                                                false
                                            }
                                        }
                                    };

                                    if merge_ok {
                                        let _ = state_changed_for_iopub.send(());
                                        let _ =
                                            broadcast_tx.send(NotebookBroadcast::DisplayUpdate {
                                                display_id: display_id.clone(),
                                                data: serde_json::to_value(&update.data)
                                                    .unwrap_or_default(),
                                                metadata: update.metadata.clone(),
                                            });
                                    }
                                }
                            }

                            JupyterMessageContent::ErrorOutput(_) => {
                                // Check if this error should go to an Output widget
                                let parent_msg_id = message
                                    .parent_header
                                    .as_ref()
                                    .map(|h| h.msg_id.as_str())
                                    .unwrap_or("");
                                if let Some(widget_comm_id) =
                                    capture_cache.get(parent_msg_id).cloned()
                                {
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Persist to CRDT as inline manifest
                                        if let Ok(manifest) = crate::output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            let manifest_json = manifest.to_json();
                                            let mut sd = state_doc_for_iopub.write().await;
                                            // Honor deferred clear_output(wait=true)
                                            if pending_clear_widgets.remove(&widget_comm_id) {
                                                sd.clear_comm_outputs(&widget_comm_id);
                                            }
                                            if sd
                                                .append_comm_output(&widget_comm_id, &manifest_json)
                                            {
                                                let _ = state_changed_for_iopub.send(());
                                            }

                                            // Read full outputs and sync to kernel
                                            if let Some(entry) = sd.get_comm(&widget_comm_id) {
                                                let output_manifests = entry.outputs.clone();

                                                // Write manifests to state.outputs for frontend
                                                let manifests_json = serde_json::Value::Array(
                                                    output_manifests.clone(),
                                                );
                                                sd.set_comm_state_property(
                                                    &widget_comm_id,
                                                    "outputs",
                                                    &manifests_json,
                                                );

                                                drop(sd);
                                                let mut resolved_outputs = Vec::new();
                                                for m in &output_manifests {
                                                    if let Ok(manifest) =
                                                        serde_json::from_value::<OutputManifest>(
                                                            m.clone(),
                                                        )
                                                    {
                                                        if let Ok(r) =
                                                            output_store::resolve_manifest(
                                                                &manifest,
                                                                &blob_store,
                                                            )
                                                            .await
                                                        {
                                                            resolved_outputs.push(r);
                                                        }
                                                    }
                                                }
                                                let _ = iopub_cmd_tx.send(QueueCommand::SendCommUpdate {
                                                    comm_id: widget_comm_id.clone(),
                                                    state: serde_json::json!({ "outputs": resolved_outputs }),
                                                }).await;
                                            }
                                        }
                                        // Frontend reads from CRDT state.outputs.
                                    }
                                    continue; // Skip normal cell output handling
                                }

                                if let Some(ref cid) = cell_id {
                                    let eid = execution_id.clone().unwrap_or_default();

                                    // Clear stream terminal state - errors break the stream chain
                                    {
                                        let mut terminals = stream_terminals.lock().await;
                                        terminals.clear(&eid);
                                    }

                                    // Convert error to nbformat JSON
                                    if let Some(nbformat_value) =
                                        message_content_to_nbformat(&message.content)
                                    {
                                        // Create manifest for error output
                                        let manifest_json = match output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            Ok(manifest) => manifest.to_json(),
                                            Err(e) => {
                                                warn!(
                                                    "[kernel-manager] Failed to create error manifest: {}",
                                                    e
                                                );
                                                nbformat_value.clone()
                                            }
                                        };

                                        // Fork RuntimeStateDoc before appending so concurrent
                                        // edits compose via CRDT merge.
                                        let mut fork = {
                                            let mut sd = state_doc_for_iopub.write().await;
                                            let mut f = sd.fork();
                                            f.set_actor(&iopub_actor_id);
                                            f
                                        };

                                        if let Err(e) = fork.append_output(&eid, &manifest_json) {
                                            warn!(
                                                "[kernel-manager] Failed to append error output to state doc: {}",
                                                e
                                            );
                                        }

                                        let merge_ok = {
                                            let mut sd = state_doc_for_iopub.write().await;
                                            if let Err(e) =
                                                crate::notebook_sync_server::catch_automerge_panic(
                                                    "iopub-error-output-merge",
                                                    || sd.merge(&mut fork),
                                                )
                                            {
                                                warn!("{}", e);
                                                sd.rebuild_from_save();
                                                false
                                            } else {
                                                let _ = state_changed_for_iopub.send(());
                                                true
                                            }
                                        };

                                        if merge_ok {
                                            let _ = broadcast_tx.send(NotebookBroadcast::Output {
                                                cell_id: cid.clone(),
                                                execution_id: eid.clone(),
                                                output_type: "error".to_string(),
                                                output_json: manifest_json.to_string(),
                                                output_index: None,
                                            });
                                        }
                                    }

                                    // Signal cell error for stop-on-error
                                    let _ = iopub_cmd_tx.try_send(QueueCommand::CellError {
                                        cell_id: cid.clone(),
                                        execution_id: eid,
                                    });
                                }
                            }

                            // Clear output - route to Output widget if capturing
                            JupyterMessageContent::ClearOutput(clear) => {
                                let parent_msg_id = message
                                    .parent_header
                                    .as_ref()
                                    .map(|h| h.msg_id.as_str())
                                    .unwrap_or("");
                                if let Some(widget_comm_id) =
                                    capture_cache.get(parent_msg_id).cloned()
                                {
                                    // Clear captured outputs in CRDT.
                                    // wait=false: clear immediately.
                                    // wait=true: defer clear until next output arrives.
                                    if clear.wait {
                                        pending_clear_widgets.insert(widget_comm_id.clone());
                                    } else {
                                        pending_clear_widgets.remove(&widget_comm_id);
                                        let mut sd = state_doc_for_iopub.write().await;
                                        if sd.clear_comm_outputs(&widget_comm_id) {
                                            let _ = state_changed_for_iopub.send(());
                                        }
                                        // Clear state.outputs for frontend
                                        sd.set_comm_state_property(
                                            &widget_comm_id,
                                            "outputs",
                                            &serde_json::json!([]),
                                        );
                                        drop(sd);
                                        // Sync empty outputs to kernel
                                        let _ = iopub_cmd_tx
                                            .send(QueueCommand::SendCommUpdate {
                                                comm_id: widget_comm_id.clone(),
                                                state: serde_json::json!({
                                                    "outputs": [],
                                                }),
                                            })
                                            .await;
                                    }
                                    // No broadcast — CRDT clear_comm_outputs handles
                                    // frontend update. For wait=true, outputs are
                                    // cleared on next append_comm_output.
                                }
                                // Note: We don't skip cell output clearing here because
                                // clear_output for non-captured outputs should still work normally
                            }

                            // Comm messages for widgets (ipywidgets protocol)
                            JupyterMessageContent::CommOpen(open) => {
                                // Extract buffers (Vec<Bytes> -> Vec<Vec<u8>>)
                                let buffers: Vec<Vec<u8>> =
                                    message.buffers.iter().map(|b| b.to_vec()).collect();

                                // Track comm state for multi-window sync
                                let data = serde_json::to_value(&open.data).unwrap_or_default();

                                let comm_open_start = std::time::Instant::now();
                                let state_json_size =
                                    serde_json::to_string(&data).map(|s| s.len()).unwrap_or(0);
                                debug!(
                                    "[comm_open] comm_id={} target={} state_size={} bytes",
                                    open.comm_id.0, open.target_name, state_json_size
                                );
                                // Store binary buffers in blob store, replacing them with
                                // {"$blob": "<hash>"} sentinels. The sentinel-carrying state
                                // is used for both the CRDT write and the broadcast — raw
                                // buffers are never sent to the frontend (avoids DataCloneError
                                // when postMessage-ing to the widget iframe).
                                let empty_obj = serde_json::json!({});
                                let state = data.get("state").unwrap_or(&empty_obj);
                                let buffer_paths = extract_buffer_paths(&data);
                                let (state_with_blobs, _used_paths) = store_widget_buffers(
                                    state,
                                    &buffer_paths,
                                    &buffers,
                                    &blob_store,
                                )
                                .await;

                                let blob_elapsed = comm_open_start.elapsed();
                                if blob_elapsed > std::time::Duration::from_millis(10) {
                                    warn!(
                                        "[iopub-timing] comm_open blob store took {:?} for comm_id={}",
                                        blob_elapsed, open.comm_id.0
                                    );
                                }

                                // Blob-store large state properties before CRDT write.
                                // This prevents catastrophic Automerge slowdown from
                                // expanding large nested JSON (e.g., 85KB Vega-Lite spec)
                                // into thousands of per-key CRDT entries.
                                let state_with_blobs =
                                    blob_store_large_state_values(&state_with_blobs, &blob_store)
                                        .await;

                                // Write to RuntimeStateDoc (native Automerge map)
                                {
                                    let lock_start = std::time::Instant::now();
                                    let model_module = state_with_blobs
                                        .get("_model_module")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let model_name = state_with_blobs
                                        .get("_model_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let seq = comm_seq.fetch_add(1, Ordering::Relaxed);
                                    let mut sd = state_doc_for_iopub.write().await;
                                    let lock_wait = lock_start.elapsed();
                                    if lock_wait > std::time::Duration::from_millis(5) {
                                        warn!(
                                            "[iopub-timing] comm_open state_doc write lock waited {:?} for comm_id={}",
                                            lock_wait, open.comm_id.0
                                        );
                                    }
                                    let crdt_start = std::time::Instant::now();
                                    sd.put_comm(
                                        &open.comm_id.0,
                                        &open.target_name,
                                        model_module,
                                        model_name,
                                        &state_with_blobs,
                                        seq,
                                    );
                                    let crdt_elapsed = crdt_start.elapsed();
                                    if crdt_elapsed > std::time::Duration::from_millis(10) {
                                        warn!(
                                            "[iopub-timing] comm_open put_comm CRDT write took {:?} for comm_id={}, state_size={} bytes",
                                            crdt_elapsed, open.comm_id.0, state_json_size
                                        );
                                    }

                                    // If this is an OutputModel with msg_id set, register capture
                                    if model_name == "OutputModel" {
                                        if let Some(msg_id) = state_with_blobs
                                            .get("msg_id")
                                            .and_then(|v| v.as_str())
                                            .filter(|s| !s.is_empty())
                                        {
                                            sd.set_comm_capture_msg_id(&open.comm_id.0, msg_id);
                                            capture_cache
                                                .insert(msg_id.to_string(), open.comm_id.0.clone());
                                        }
                                    }

                                    let _ = state_changed_for_iopub.send(());
                                }

                                // No broadcast — frontend receives comm_open via
                                // CRDT sync of RuntimeStateDoc comms map.
                                let total = comm_open_start.elapsed();
                                if total > std::time::Duration::from_millis(50) {
                                    warn!(
                                        "[iopub-timing] comm_open TOTAL {:?} for comm_id={} target={} state_size={} bytes",
                                        total, open.comm_id.0, open.target_name, state_json_size
                                    );
                                }
                            }

                            JupyterMessageContent::CommMsg(msg) => {
                                // Serialize the content to JSON
                                let content =
                                    serde_json::to_value(&message.content).unwrap_or_default();

                                // Extract buffers (Vec<Bytes> -> Vec<Vec<u8>>)
                                let buffers: Vec<Vec<u8>> =
                                    message.buffers.iter().map(|b| b.to_vec()).collect();

                                // Track state updates (method="update") for multi-window sync
                                let data = serde_json::to_value(&msg.data).unwrap_or_default();
                                let method = data.get("method").and_then(|m| m.as_str());

                                let comm_msg_start = std::time::Instant::now();
                                debug!("[comm_msg] comm_id={} method={:?}", msg.comm_id.0, method);
                                if method == Some("update") {
                                    if let Some(state_delta) = data.get("state") {
                                        // If this update changes msg_id on an Output widget,
                                        // update capture routing in CRDT + cache.
                                        if let Some(new_msg_id) =
                                            state_delta.get("msg_id").and_then(|v| v.as_str())
                                        {
                                            // Remove old capture for this comm_id
                                            capture_cache.retain(|_, cid| cid != &msg.comm_id.0);

                                            // Set new capture if non-empty (single-depth only)
                                            if !new_msg_id.is_empty() {
                                                if let Some(existing) =
                                                    capture_cache.get(new_msg_id)
                                                {
                                                    warn!(
                                                        "[comm_msg] Nested capture: {} overrides {} for msg_id={}",
                                                        msg.comm_id.0, existing, new_msg_id
                                                    );
                                                }
                                                capture_cache.insert(
                                                    new_msg_id.to_string(),
                                                    msg.comm_id.0.clone(),
                                                );
                                            }

                                            // Write to CRDT
                                            let mut sd = state_doc_for_iopub.write().await;
                                            sd.set_comm_capture_msg_id(&msg.comm_id.0, new_msg_id);
                                            let _ = state_changed_for_iopub.send(());
                                        }

                                        // Send state delta to coalescing writer for
                                        // batched CRDT updates (16ms window).
                                        // Binary buffers get blob-sentinel treatment
                                        // so the CRDT stores hashes, not raw bytes.
                                        let coalesce_delta = if !buffers.is_empty() {
                                            let buffer_paths = extract_buffer_paths(&data);
                                            let (state_with_blobs, _) = store_widget_buffers(
                                                state_delta,
                                                &buffer_paths,
                                                &buffers,
                                                &blob_store,
                                            )
                                            .await;
                                            state_with_blobs
                                        } else {
                                            state_delta.clone()
                                        };
                                        if let Some(ref tx) = comm_coalesce_tx {
                                            let _ =
                                                tx.send((msg.comm_id.0.clone(), coalesce_delta));
                                        }
                                    }
                                }

                                let comm_msg_elapsed = comm_msg_start.elapsed();
                                if comm_msg_elapsed > std::time::Duration::from_millis(10) {
                                    warn!(
                                        "[iopub-timing] comm_msg took {:?} for comm_id={} method={:?}",
                                        comm_msg_elapsed, msg.comm_id.0, method
                                    );
                                }

                                // State updates flow through the CRDT (coalescing writer).
                                // Custom messages (buttons, model.send()) still need broadcast
                                // since they are ephemeral events, not persistent state.
                                if method != Some("update") {
                                    let _ = broadcast_tx.send(NotebookBroadcast::Comm {
                                        msg_type: message.header.msg_type.clone(),
                                        content: content.clone(),
                                        buffers: buffers.clone(),
                                    });
                                }
                            }

                            JupyterMessageContent::CommClose(close) => {
                                debug!("[kernel-manager] comm_close: comm_id={}", close.comm_id.0);

                                // Log total IOPub message processing time for slow messages
                                let iopub_elapsed = iopub_start.elapsed();
                                if iopub_elapsed > std::time::Duration::from_millis(50) {
                                    warn!(
                                        "[iopub-timing] message type={} took {:?} total",
                                        msg_type, iopub_elapsed
                                    );
                                }

                                // Remove from capture cache
                                capture_cache.retain(|_, cid| cid != &close.comm_id.0);

                                // Remove from RuntimeStateDoc — frontend detects
                                // removal via CRDT watcher and synthesizes comm_close.
                                {
                                    let mut sd = state_doc_for_iopub.write().await;
                                    if sd.remove_comm(&close.comm_id.0) {
                                        let _ = state_changed_for_iopub.send(());
                                    }
                                }
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
            // Iopub loop exited — kernel is dead. Signal the queue processor
            // so it can unblock the execution queue and notify the frontend.
            warn!("[kernel-manager] iopub loop exited, signaling KernelDied");
            let _ = iopub_cmd_tx.try_send(QueueCommand::KernelDied);
        });

        // Create shell connection
        let identity = runtimelib::peer_identity_for_session(&self.session_id)?;
        let mut shell = runtimelib::create_client_shell_connection_with_identity(
            &connection_info,
            &self.session_id,
            identity,
        )
        .await?;

        // Verify kernel is alive — race kernel_info_reply against process death
        let request: JupyterMessage = KernelInfoRequest::default().into();
        shell.send(request).await?;

        let reply = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(30), shell.read()) => {
                match result {
                    Ok(Ok(msg)) => Ok(msg),
                    Ok(Err(e)) => Err(anyhow::anyhow!("Kernel did not respond: {}", e)),
                    Err(_) => Err(anyhow::anyhow!("Kernel did not respond within 30s")),
                }
            }
            died_msg = died_rx => {
                let msg = died_msg.unwrap_or_else(|_| "unknown".to_string());
                Err(anyhow::anyhow!("Kernel process died before responding: {}", msg))
            }
        };

        match reply {
            Ok(msg) => {
                info!(
                    "[kernel-manager] Kernel alive: got {} reply",
                    msg.header.msg_type
                );
            }
            Err(e) => {
                error!("[kernel-manager] {}", e);
                // Abort process watcher to kill orphaned kernel
                if let Some(task) = self.process_watcher_task.take() {
                    task.abort();
                }
                return Err(e);
            }
        }

        // Split shell into reader/writer
        let (shell_writer, mut shell_reader) = shell.split();

        // Spawn shell reader task
        let shell_broadcast_tx = self.broadcast_tx.clone();
        let shell_cell_id_map = self.cell_id_map.clone();
        let shell_pending_history = self.pending_history.clone();
        let shell_pending_completions = self.pending_completions.clone();
        let shell_state_doc = self.state_doc.clone();
        let shell_state_changed_tx = self.state_changed_tx.clone();
        let shell_blob_store = self.blob_store.clone();
        let iopub_actor_id = self.kernel_actor_id.clone();

        let shell_reader_task = tokio::spawn(async move {
            loop {
                match shell_reader.read().await {
                    Ok(msg) => {
                        let _parent_msg_id = msg.parent_header.as_ref().map(|h| h.msg_id.clone());

                        match msg.content {
                            JupyterMessageContent::ExecuteReply(ref reply) => {
                                // Get (cell_id, execution_id) from msg_id mapping
                                let cell_entry = msg.parent_header.as_ref().and_then(|h| {
                                    shell_cell_id_map.lock().ok()?.get(&h.msg_id).cloned()
                                });
                                let cell_id = cell_entry.as_ref().map(|(cid, _)| cid.clone());
                                let execution_id = cell_entry.as_ref().map(|(_, eid)| eid.clone());

                                // Process page payloads - convert to display_data outputs
                                // This handles IPython's ? and ?? help commands
                                if let Some(ref cid) = cell_id {
                                    for payload in &reply.payload {
                                        if let jupyter_protocol::Payload::Page { data, .. } =
                                            payload
                                        {
                                            // Convert Media to nbformat display_data
                                            let nbformat_value = media_to_display_data(data);

                                            // Create manifest for page output
                                            let manifest_json = match output_store::create_manifest(
                                                &nbformat_value,
                                                &shell_blob_store,
                                                DEFAULT_INLINE_THRESHOLD,
                                            )
                                            .await
                                            {
                                                Ok(manifest) => manifest.to_json(),
                                                Err(e) => {
                                                    warn!(
                                                        "[kernel-manager] Failed to create page manifest: {}",
                                                        e
                                                    );
                                                    nbformat_value.clone()
                                                }
                                            };

                                            // Fork RuntimeStateDoc before appending so
                                            // concurrent edits compose via CRDT merge.
                                            let eid = execution_id.clone().unwrap_or_default();
                                            let mut fork = {
                                                let mut sd = shell_state_doc.write().await;
                                                let mut f = sd.fork();
                                                f.set_actor(&iopub_actor_id);
                                                f
                                            };

                                            if let Err(e) = fork.append_output(&eid, &manifest_json)
                                            {
                                                warn!(
                                                    "[kernel-manager] Failed to append page output to state doc: {}",
                                                    e
                                                );
                                            }

                                            let merge_ok = {
                                                let mut sd = shell_state_doc.write().await;
                                                if let Err(e) = crate::notebook_sync_server::catch_automerge_panic(
                                                    "shell-output-merge",
                                                    || sd.merge(&mut fork),
                                                ) {
                                                    warn!("{}", e);
                                                    sd.rebuild_from_save();
                                                    false
                                                } else {
                                                    let _ = shell_state_changed_tx.send(());
                                                    true
                                                }
                                            };

                                            if merge_ok {
                                                // Broadcast to all windows
                                                let _ = shell_broadcast_tx.send(
                                                    NotebookBroadcast::Output {
                                                        cell_id: cid.clone(),
                                                        execution_id: execution_id
                                                            .clone()
                                                            .unwrap_or_default(),
                                                        output_type: "display_data".to_string(),
                                                        output_json: manifest_json.to_string(),
                                                        output_index: None,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }

                                // Broadcast execution done for error status
                                if reply.status != jupyter_protocol::ReplyStatus::Ok {
                                    if let Some(ref cid) = cell_id {
                                        let _ = shell_broadcast_tx.send(
                                            NotebookBroadcast::ExecutionDone {
                                                cell_id: cid.clone(),
                                                execution_id: execution_id
                                                    .clone()
                                                    .unwrap_or_default(),
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
                            JupyterMessageContent::CompleteReply(ref reply) => {
                                // Get the parent msg_id to find the pending request
                                if let Some(ref parent) = msg.parent_header {
                                    let msg_id = &parent.msg_id;
                                    if let Ok(mut pending) = shell_pending_completions.lock() {
                                        if let Some(tx) = pending.remove(msg_id) {
                                            // Convert kernel matches to CompletionItem (LSP-ready format)
                                            let items: Vec<CompletionItem> = reply
                                                .matches
                                                .iter()
                                                .map(|m| CompletionItem {
                                                    label: m.clone(),
                                                    kind: None,
                                                    detail: None,
                                                    source: Some("kernel".to_string()),
                                                })
                                                .collect();

                                            debug!(
                                                "[kernel-manager] Resolved completion request: {} items",
                                                items.len()
                                            );
                                            let _ = tx.send((
                                                items,
                                                reply.cursor_start,
                                                reply.cursor_end,
                                            ));
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

        // Spawn heartbeat monitor - detects unresponsive kernel (alive but hung)
        let hb_cmd_tx = cmd_tx.clone();
        let hb_conn_info = connection_info.clone();
        let heartbeat_task = tokio::spawn(async move {
            // Give kernel time to fully initialize
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                let check = async {
                    let mut hb =
                        runtimelib::create_client_heartbeat_connection(&hb_conn_info).await?;
                    hb.single_heartbeat().await
                };

                match tokio::time::timeout(std::time::Duration::from_secs(3), check).await {
                    Ok(Ok(())) => {
                        // Kernel is alive, continue monitoring
                    }
                    Ok(Err(e)) => {
                        warn!("[kernel-manager] Heartbeat connection error: {}", e);
                        let _ = hb_cmd_tx.try_send(QueueCommand::KernelDied);
                        break;
                    }
                    Err(_) => {
                        warn!("[kernel-manager] Heartbeat timeout, kernel unresponsive");
                        let _ = hb_cmd_tx.try_send(QueueCommand::KernelDied);
                        break;
                    }
                }
            }
        });

        // Spawn coalesced comm state writer — batches comm_msg(update) deltas
        // into periodic CRDT writes (16ms window) to keep RuntimeStateDoc current
        // without overwhelming the sync pipeline during rapid slider drags.
        // Channel was created earlier (before IOPub task) so both tasks share it.
        let mut coalesce_rx = coalesce_rx;
        let coalesce_state_doc = self.state_doc.clone();
        let coalesce_state_changed = self.state_changed_tx.clone();
        let coalesce_blob_store = self.blob_store.clone();
        let comm_coalesce_task = tokio::spawn(async move {
            let mut pending: HashMap<String, serde_json::Value> = HashMap::new();
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(16));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    msg = coalesce_rx.recv() => {
                        match msg {
                            Some((comm_id, delta)) => {
                                // Merge delta into pending accumulator (last-write-wins per key)
                                let entry = pending.entry(comm_id)
                                    .or_insert_with(|| serde_json::json!({}));
                                if let (Some(existing), Some(new)) =
                                    (entry.as_object_mut(), delta.as_object())
                                {
                                    for (k, v) in new {
                                        existing.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            None => break, // Channel closed, kernel shutting down
                        }
                    }
                    _ = timer.tick() => {
                        if pending.is_empty() {
                            continue;
                        }
                        let mut batch = std::mem::take(&mut pending);
                        // Blob-store large delta values before CRDT merge.
                        for delta in batch.values_mut() {
                            *delta = blob_store_large_state_values(delta, &coalesce_blob_store).await;
                        }
                        let mut sd = coalesce_state_doc.write().await;
                        let mut any_changed = false;
                        for (comm_id, delta) in &batch {
                            if sd.merge_comm_state_delta(comm_id, delta) {
                                any_changed = true;
                            }
                        }
                        if any_changed {
                            let _ = coalesce_state_changed.send(());
                        }
                    }
                }
            }
        });

        // Store state (process_watcher_task already stored immediately after spawn)
        self.connection_info = Some(connection_info);
        self.connection_file = Some(connection_file_path);
        self.iopub_task = Some(iopub_task);
        self.shell_reader_task = Some(shell_reader_task);
        self.heartbeat_task = Some(heartbeat_task);
        self.comm_coalesce_tx = Some(coalesce_tx);
        self.comm_coalesce_task = Some(comm_coalesce_task);
        self.shell_writer = Some(shell_writer);
        self.status = KernelStatus::Idle;

        // Broadcast idle status
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "idle".to_string(),
            cell_id: None,
        });

        {
            let mut sd = self.state_doc.write().await;
            if sd.set_kernel_status("idle") {
                let _ = self.state_changed_tx.send(());
            }
        }

        info!("[kernel-manager] Kernel started: {}", kernel_id);
        Ok(())
    }

    /// Queue a cell for execution with a pre-assigned execution_id.
    ///
    /// Used by the agent subprocess where the coordinator already generated
    /// and stamped the execution_id on the cell in NotebookDoc.
    pub async fn queue_cell_with_id(
        &mut self,
        cell_id: String,
        code: String,
        execution_id: String,
    ) -> Result<String> {
        self.queue_cell_inner(cell_id, code, Some(execution_id))
            .await
    }

    /// Queue a cell for execution.
    ///
    /// Idempotent: if the cell is already executing or queued, returns the
    /// existing `execution_id` instead of generating a new one.
    /// Returns the `execution_id` for this execution.
    pub async fn queue_cell(&mut self, cell_id: String, code: String) -> Result<String> {
        self.queue_cell_inner(cell_id, code, None).await
    }

    /// Internal queue implementation. If `execution_id` is None, generates a new one.
    async fn queue_cell_inner(
        &mut self,
        cell_id: String,
        code: String,
        execution_id: Option<String>,
    ) -> Result<String> {
        // Idempotent: return existing execution_id if already executing or queued
        if let Some((ref cid, ref eid)) = self.executing {
            if cid == &cell_id {
                info!(
                    "[kernel-manager] Cell {} already executing ({}), skipping",
                    cell_id, eid
                );
                return Ok(eid.clone());
            }
        }
        if let Some(existing) = self.queue.iter().find(|c| c.cell_id == cell_id) {
            info!(
                "[kernel-manager] Cell {} already queued ({}), skipping",
                cell_id, existing.execution_id
            );
            return Ok(existing.execution_id.clone());
        }

        let execution_id = execution_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        info!(
            "[kernel-manager] Queuing cell: {} (execution_id={})",
            cell_id, execution_id
        );

        // Add to queue
        self.queue.push_back(QueuedCell {
            cell_id: cell_id.clone(),
            execution_id: execution_id.clone(),
            code,
        });

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing_entry(),
            queued: self.queued_cells(),
        });

        {
            let doc_exec = self.executing_entry().as_ref().map(to_doc_entry);
            let doc_queued = to_doc_entries(&self.queued_cells());
            let mut sd = self.state_doc.write().await;
            let mut changed = sd.create_execution(&execution_id, &cell_id);
            changed |= sd.set_queue(doc_exec.as_ref(), &doc_queued);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Try to process if nothing executing
        self.process_next().await?;
        Ok(execution_id)
    }

    /// Clear terminal state for an execution.
    pub async fn clear_outputs(&self, execution_id: &str) {
        info!(
            "[kernel-manager] Clearing terminal state for execution: {}",
            execution_id
        );
        let mut terminals = self.stream_terminals.lock().await;
        terminals.clear(execution_id);
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

        self.executing = Some((cell.cell_id.clone(), cell.execution_id.clone()));
        self.status = KernelStatus::Busy;

        // Collect queue state before borrowing shell_writer
        let executing = self.executing_entry();
        let queued = self.queued_cells();

        // Broadcast queue state
        let _ = self
            .broadcast_tx
            .send(NotebookBroadcast::QueueChanged { executing, queued });

        {
            let doc_exec = self.executing_entry().as_ref().map(to_doc_entry);
            let doc_queued = to_doc_entries(&self.queued_cells());
            let mut sd = self.state_doc.write().await;
            let mut changed = sd.set_execution_running(&cell.execution_id);
            changed |= sd.set_queue(doc_exec.as_ref(), &doc_queued);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Send execute request
        let request = ExecuteRequest::new(cell.code.clone());
        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        // Register msg_id → (cell_id, execution_id) BEFORE sending.
        // First, remove any old mappings for this cell_id (from previous executions).
        // This bounds the map to one entry per cell, not per execution, while still
        // allowing both shell (execute_reply) and iopub (idle status) to use the mapping.
        {
            let mut map = self.cell_id_map.lock().unwrap();
            map.retain(|_, (cid, _)| cid != &cell.cell_id);
            map.insert(
                msg_id.clone(),
                (cell.cell_id.clone(), cell.execution_id.clone()),
            );
        }

        // Now borrow shell_writer mutably
        let shell = self.shell_writer.as_mut().unwrap();
        shell.send(message).await?;
        info!(
            "[kernel-manager] Sent execute_request: msg_id={} cell_id={} execution_id={}",
            msg_id, cell.cell_id, cell.execution_id
        );

        Ok(())
    }

    /// Send a comm_msg(update) to the kernel via the shell channel.
    ///
    /// Used to sync Output widget captured outputs directly to the kernel
    /// without a frontend round-trip.
    pub async fn send_comm_update(
        &mut self,
        comm_id: &str,
        state: serde_json::Value,
    ) -> Result<()> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("shell_writer not available"))?;

        let comm_msg = jupyter_protocol::CommMsg {
            comm_id: jupyter_protocol::CommId(comm_id.to_string()),
            data: {
                let mut map = serde_json::Map::new();
                map.insert("method".to_string(), serde_json::json!("update"));
                map.insert("state".to_string(), state);
                map.insert("buffer_paths".to_string(), serde_json::json!([]));
                map
            },
        };
        let message: jupyter_protocol::JupyterMessage = comm_msg.into();
        shell.send(message).await?;
        debug!(
            "[kernel-manager] Sent comm_msg(update) to kernel: comm_id={}",
            comm_id
        );
        Ok(())
    }

    /// Record that the current execution produced an error output.
    ///
    /// Called by the sync server's `CellError` handler before `execution_done`.
    pub fn mark_execution_error(&mut self) {
        self.execution_had_error = true;
    }

    /// Mark a cell execution as complete and process next.
    pub async fn execution_done(&mut self, cell_id: &str, execution_id: &str) -> Result<()> {
        let matches = self
            .executing
            .as_ref()
            .is_some_and(|(cid, _)| cid == cell_id);
        if matches {
            let success = !self.execution_had_error;
            self.executing = None;
            self.execution_had_error = false;
            self.status = KernelStatus::Idle;

            // Note: cell_id_map cleanup happens when a cell is RE-EXECUTED (in
            // process_next), not here. The shell and iopub channels race,
            // and both need the mapping. Cleaning up on re-execution bounds the map
            // to one entry per cell while avoiding the race condition.

            // Broadcast done
            let _ = self.broadcast_tx.send(NotebookBroadcast::ExecutionDone {
                cell_id: cell_id.to_string(),
                execution_id: execution_id.to_string(),
            });

            // Broadcast queue state
            let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
                executing: None,
                queued: self.queued_cells(),
            });

            {
                let doc_queued = to_doc_entries(&self.queued_cells());
                let mut sd = self.state_doc.write().await;
                let mut changed = sd.set_execution_done(execution_id, success);
                changed |= sd.set_queue(None, &doc_queued);
                changed |= sd.trim_executions(MAX_EXECUTION_ENTRIES) > 0;
                if changed {
                    let _ = self.state_changed_tx.send(());
                }
            }

            // Process next
            self.process_next().await?;
        }
        Ok(())
    }

    /// Handle kernel death (process exit or heartbeat failure).
    ///
    /// Unblocks the execution queue by clearing the executing cell and queue,
    /// and broadcasts an error status to all connected peers.
    ///
    /// Returns the interrupted execution (if any) and the cleared queue entries.
    ///
    /// This method is idempotent - multiple calls (e.g., from both process
    /// watcher and heartbeat monitor) are safe.
    pub fn kernel_died(&mut self) -> (Option<(String, String)>, Vec<QueueEntry>) {
        // Idempotent: if already dead, don't re-broadcast
        if self.status == KernelStatus::Dead {
            debug!("[kernel-manager] kernel_died called but already dead, ignoring");
            return (None, vec![]);
        }

        warn!(
            "[kernel-manager] Kernel died, executing={:?}, queued={}",
            self.executing,
            self.queue.len()
        );

        // Capture the interrupted execution before clearing
        let interrupted = self.executing.take();
        self.status = KernelStatus::Dead;

        // Clear any queued cells — they can't execute without a kernel
        let cleared = self.clear_queue();
        if !cleared.is_empty() {
            info!(
                "[kernel-manager] Cleared {} queued cells due to kernel death",
                cleared.len()
            );
        }

        // Broadcast error status to all peers
        // Note: status must be exactly "error" to match frontend's known statuses
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "error".to_string(),
            cell_id: None,
        });

        // Broadcast empty queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: None,
            queued: vec![],
        });

        // Note: state_doc writes for kernel_died happen in the async command
        // processor (notebook_sync_server.rs QueueCommand::KernelDied handler).
        // state_doc.set_kernel_status("error") + set_queue(None, &[])
        // + set_execution_done for interrupted + cleared entries

        (interrupted, cleared)
    }

    /// Interrupt the currently executing cell and clear the execution queue.
    pub async fn interrupt(&mut self) -> Result<()> {
        let connection_info = self
            .connection_info
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let mut control =
            runtimelib::create_client_control_connection(connection_info, &self.session_id).await?;

        let request: JupyterMessage = InterruptRequest {}.into();
        control.send(request).await?;

        info!("[kernel-manager] Sent interrupt_request");

        // Clear the execution queue - interrupt semantically means "stop all pending work"
        let cleared = self.clear_queue();
        if !cleared.is_empty() {
            info!(
                "[kernel-manager] Cleared {} queued cells due to interrupt",
                cleared.len()
            );
        }

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

        let glob_pattern = escape_glob_pattern(pattern.as_deref());
        let request = HistoryRequest::Search {
            pattern: glob_pattern,
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

    /// Request code completions from the kernel.
    ///
    /// Sends a complete_request to the kernel and waits for the reply.
    /// Returns an error if no kernel is running or the request times out.
    pub async fn complete(
        &mut self,
        code: String,
        cursor_pos: usize,
    ) -> Result<(Vec<CompletionItem>, usize, usize)> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        // Create completion request
        let request = CompleteRequest { code, cursor_pos };

        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        // Create response channel
        let (tx, rx) = oneshot::channel();

        // Register pending request BEFORE sending
        self.pending_completions
            .lock()
            .map_err(|_| anyhow::anyhow!("Lock poisoned"))?
            .insert(msg_id.clone(), tx);

        // Send request; clean up pending entry on failure
        if let Err(e) = shell.send(message).await {
            if let Ok(mut pending) = self.pending_completions.lock() {
                pending.remove(&msg_id);
            }
            return Err(e.into());
        }
        debug!("[kernel-manager] Sent complete_request: msg_id={}", msg_id);

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => {
                // Channel closed without response
                Err(anyhow::anyhow!("Completion request cancelled"))
            }
            Err(_) => {
                // Timeout - clean up pending request
                if let Ok(mut pending) = self.pending_completions.lock() {
                    pending.remove(&msg_id);
                }
                Err(anyhow::anyhow!("Completion request timed out"))
            }
        }
    }

    /// Clear the execution queue.
    pub fn clear_queue(&mut self) -> Vec<QueueEntry> {
        let cleared: Vec<QueueEntry> = self
            .queue
            .drain(..)
            .map(|c| QueueEntry {
                cell_id: c.cell_id,
                execution_id: c.execution_id,
            })
            .collect();

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing_entry(),
            queued: vec![],
        });

        // Note: state_doc writes for clear_queue happen in the async command
        // processor (notebook_sync_server.rs QueueCommand::CellError handler).
        // state_doc.set_queue(self.executing_entry().as_ref(), &[])

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

        {
            let mut sd = self.state_doc.write().await;
            let mut changed = false;
            changed |= sd.set_kernel_status("shutdown");
            changed |= sd.set_queue(None, &[]);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Abort tasks
        if let Some(task) = self.iopub_task.take() {
            task.abort();
        }
        if let Some(task) = self.shell_reader_task.take() {
            task.abort();
        }
        if let Some(task) = self.process_watcher_task.take() {
            task.abort();
        }
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        // Drop sender first so the coalescing task exits, then abort
        self.comm_coalesce_tx.take();
        if let Some(task) = self.comm_coalesce_task.take() {
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
                match e {
                    // Process group already gone — nothing to do
                    nix::errno::Errno::ESRCH => {}
                    // Permission denied — pgid may have been reused or is no longer ours
                    nix::errno::Errno::EPERM => {
                        debug!(
                            "[kernel-manager] Permission denied killing process group {}",
                            pgid
                        );
                    }
                    other => {
                        error!(
                            "[kernel-manager] Failed to kill process group {}: {}",
                            pgid, other
                        );
                    }
                }
            }
        }

        // Unregister from process registry
        if let Some(kid) = self.kernel_id.take() {
            crate::kernel_pids::unregister_kernel(&kid);
        }

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
        if let Some(task) = self.process_watcher_task.take() {
            task.abort();
        }
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        self.comm_coalesce_tx.take();
        if let Some(task) = self.comm_coalesce_task.take() {
            task.abort();
        }

        // Kill process group on Unix
        #[cfg(unix)]
        if let Some(pgid) = self.process_group_id.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }

        // Unregister from process registry
        if let Some(kid) = self.kernel_id.take() {
            crate::kernel_pids::unregister_kernel(&kid);
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
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));
        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _state_changed_rx) = broadcast::channel(16);
        let presence = Arc::new(RwLock::new(PresenceState::new()));
        let (presence_tx, _presence_rx) = broadcast::channel(16);
        let kernel = RoomKernel::new(
            tx,
            blob_store,
            state_doc,
            state_changed_tx,
            presence,
            presence_tx,
        );

        assert!(!kernel.is_running());
        assert!(kernel.executing_cell().is_none());
        assert!(kernel.queued_cells().is_empty());
        assert_eq!(kernel.status(), KernelStatus::Starting);
    }

    #[test]
    fn test_escape_glob_pattern_none() {
        assert_eq!(escape_glob_pattern(None), "*");
    }

    #[test]
    fn test_escape_glob_pattern_empty() {
        assert_eq!(escape_glob_pattern(Some("")), "*");
    }

    #[test]
    fn test_escape_glob_pattern_simple() {
        assert_eq!(escape_glob_pattern(Some("for")), "*for*");
        assert_eq!(escape_glob_pattern(Some("import time")), "*import time*");
    }

    #[test]
    fn test_escape_glob_pattern_metacharacters() {
        // Each glob metacharacter should be wrapped in brackets to escape it
        assert_eq!(escape_glob_pattern(Some("*")), "*[*]*");
        assert_eq!(escape_glob_pattern(Some("?")), "*[?]*");
        assert_eq!(escape_glob_pattern(Some("[test]")), "*[[]test[]]*");
    }

    #[test]
    fn test_escape_glob_pattern_mixed() {
        // Complex pattern with multiple metacharacters
        assert_eq!(escape_glob_pattern(Some("a*b?c[d]")), "*a[*]b[?]c[[]d[]]*");
    }

    // ── update_output_by_display_id_with_manifests tests ──────────────

    use tempfile::TempDir;

    fn test_blob_store(dir: &TempDir) -> BlobStore {
        BlobStore::new(dir.path().join("blobs"))
    }

    /// Helper: create a display_data manifest with a display_id and append
    /// the inline manifest to the given execution in the state doc.
    async fn insert_display_output(
        state_doc: &mut RuntimeStateDoc,
        execution_id: &str,
        display_id: &str,
        text_content: &str,
        blob_store: &BlobStore,
    ) -> serde_json::Value {
        let nbformat = serde_json::json!({
            "output_type": "display_data",
            "data": { "text/plain": text_content },
            "metadata": {},
            "transient": { "display_id": display_id }
        });
        let manifest =
            output_store::create_manifest(&nbformat, blob_store, DEFAULT_INLINE_THRESHOLD)
                .await
                .unwrap();
        let manifest_json = manifest.to_json();
        state_doc
            .append_output(execution_id, &manifest_json)
            .unwrap();
        manifest_json
    }

    /// Extract the text/plain inline content from an output manifest Value.
    fn read_text_plain(manifest: &serde_json::Value) -> String {
        manifest["data"]["text/plain"]["inline"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn test_update_display_id_updates_all_matching_outputs() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        // Two executions, both with the same display_id (simulates re-running a cell)
        state_doc.create_execution("exec-1", "cell-a");
        state_doc.create_execution("exec-2", "cell-a");
        insert_display_output(&mut state_doc, "exec-1", "progress", "old", &store).await;
        insert_display_output(&mut state_doc, "exec-2", "progress", "old", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "progress",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(result, "should report that outputs were updated");

        // Both executions' outputs should now contain "updated"
        let outputs_1 = state_doc.get_outputs("exec-1");
        let outputs_2 = state_doc.get_outputs("exec-2");
        assert_eq!(outputs_1.len(), 1);
        assert_eq!(outputs_2.len(), 1);
        assert_eq!(read_text_plain(&outputs_1[0]), "updated");
        assert_eq!(read_text_plain(&outputs_2[0]), "updated");
    }

    #[tokio::test]
    async fn test_update_display_id_no_match_returns_false() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        state_doc.create_execution("exec-1", "cell-a");
        insert_display_output(&mut state_doc, "exec-1", "progress", "hello", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "nonexistent-id",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(!result, "should return false when no display_id matches");

        // Original output unchanged
        let outputs = state_doc.get_outputs("exec-1");
        assert_eq!(read_text_plain(&outputs[0]), "hello");
    }

    #[tokio::test]
    async fn test_update_display_id_only_updates_matching() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        state_doc.create_execution("exec-1", "cell-a");
        state_doc.create_execution("exec-2", "cell-b");
        insert_display_output(&mut state_doc, "exec-1", "progress", "match-me", &store).await;
        insert_display_output(&mut state_doc, "exec-2", "other-id", "leave-me", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "progress",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(result);

        // Only the matching output should be updated
        let outputs_1 = state_doc.get_outputs("exec-1");
        let outputs_2 = state_doc.get_outputs("exec-2");
        assert_eq!(read_text_plain(&outputs_1[0]), "updated");
        assert_eq!(read_text_plain(&outputs_2[0]), "leave-me");
    }
}
