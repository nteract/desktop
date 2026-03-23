//! Notebook-specific protocol types extracted from runtimed.
//!
//! Pure data definitions (structs and enums) for the notebook sync protocol.
//! No `impl` blocks — just shapes + serde derives.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Data structs referenced by protocol enums ───────────────────────────────

/// A snapshot of a comm channel's state.
///
/// Stored in the daemon and sent to newly connected clients so they can
/// reconstruct widget models that were created before they connected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommSnapshot {
    /// The comm_id (unique identifier for this comm channel).
    pub comm_id: String,

    /// Target name (e.g., "jupyter.widget", "jupyter.widget.version").
    pub target_name: String,

    /// Current state snapshot (merged from all updates).
    /// For widgets, this contains the full model state.
    pub state: serde_json::Value,

    /// Model module (e.g., "@jupyter-widgets/controls", "anywidget").
    /// Extracted from `_model_module` in state for convenience.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_module: Option<String>,

    /// Model name (e.g., "IntSliderModel", "AnyModel").
    /// Extracted from `_model_name` in state for convenience.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,

    /// Binary buffers associated with this comm (e.g., for images, arrays).
    /// Stored inline for simplicity; large buffers could be moved to blob store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buffers: Vec<Vec<u8>>,
}

/// Environment configuration captured at kernel launch time.
/// Used to detect when notebook metadata has drifted from the running kernel.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LaunchedEnvConfig {
    /// UV inline deps (if env_source is "uv:inline")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_deps: Option<Vec<String>>,

    /// Conda inline deps (if env_source is "conda:inline")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conda_deps: Option<Vec<String>>,

    /// Conda channels (if env_source is "conda:inline")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conda_channels: Option<Vec<String>>,

    /// Deno config (if kernel_type is "deno")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deno_config: Option<DenoLaunchedConfig>,

    /// Path to the venv used by the kernel (for hot-sync into running env)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venv_path: Option<PathBuf>,

    /// Path to python executable (for hot-sync, avoids hardcoding bin/python vs Scripts/python.exe)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_path: Option<PathBuf>,

    /// Unique identifier for this kernel launch session.
    /// Used to detect if kernel was swapped during async operations (e.g., hot-sync).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
}

/// Deno configuration captured at kernel launch time.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DenoLaunchedConfig {
    /// Deno permission flags
    #[serde(default)]
    pub permissions: Vec<String>,

    /// Path to import_map.json
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_map: Option<String>,

    /// Path to deno.json config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,

    /// Whether npm: imports auto-install packages
    #[serde(default = "default_flexible_npm")]
    pub flexible_npm_imports: bool,
}

fn default_flexible_npm() -> bool {
    true
}

/// Error information for a pool that is failing to warm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolError {
    /// Human-readable error message.
    pub message: String,
    /// Package that failed to install (if identified).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_package: Option<String>,
    /// Number of consecutive failures.
    pub consecutive_failures: u32,
    /// Seconds until next retry (0 if retry is imminent).
    pub retry_in_secs: u64,
}

/// An entry in the execution queue, pairing a cell with its execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueEntry {
    pub cell_id: String,
    pub execution_id: String,
}

// ── Helper structs ──────────────────────────────────────────────────────────

/// A single entry from kernel input history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Session number (0 for current session)
    pub session: i32,
    /// Line number within the session
    pub line: i32,
    /// The source code that was executed
    pub source: String,
}

/// A single completion item (LSP-ready structure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    /// The completion text
    pub label: String,
    /// Kind: "function", "variable", "class", "module", etc.
    /// Populated by LSP later; kernel completions leave this as None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Short type annotation (e.g. "def read_csv(filepath_or_buffer, ...)")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Source: "kernel" now, "ruff"/"basedpyright" later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Difference between launched environment config and current metadata.
/// Used to show the user what packages would be added/removed on restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvSyncDiff {
    /// Packages to add (in current metadata but not in launched config).
    #[serde(default)]
    pub added: Vec<String>,
    /// Packages to remove (in launched config but not in current metadata).
    #[serde(default)]
    pub removed: Vec<String>,
    /// Conda channels changed (requires restart to use new channels).
    #[serde(default)]
    pub channels_changed: bool,
    /// Deno config changed (permissions, import_map, etc.)
    #[serde(default)]
    pub deno_changed: bool,
}

// ── Notebook protocol enums ─────────────────────────────────────────────────

/// Requests sent from notebook app to daemon for notebook operations.
///
/// These are sent as JSON over the notebook sync connection alongside
/// Automerge sync messages. The daemon handles kernel lifecycle and
/// execution, becoming the single source of truth for outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NotebookRequest {
    /// Launch a kernel for this notebook room.
    /// If a kernel is already running, returns info about the existing kernel.
    LaunchKernel {
        /// Kernel type: "python" or "deno"
        kernel_type: String,
        /// Environment source: "uv:inline", "conda:prewarmed", etc.
        env_source: String,
        /// Path to the notebook file (for working directory)
        notebook_path: Option<String>,
    },

    /// Queue a cell for execution.
    /// Daemon adds to queue and executes when previous cells complete.
    #[deprecated(
        since = "0.1.0",
        note = "Use ExecuteCell instead - reads source from synced document"
    )]
    QueueCell { cell_id: String, code: String },

    /// Execute a cell by reading its source from the automerge doc.
    /// This is the preferred method - ensures execution matches synced document state.
    ExecuteCell { cell_id: String },

    /// Clear outputs for a cell (before re-execution).
    ClearOutputs { cell_id: String },

    /// Interrupt the currently executing cell.
    InterruptExecution {},

    /// Shutdown the kernel for this room.
    ShutdownKernel {},

    /// Get info about the current kernel (if any).
    GetKernelInfo {},

    /// Get the execution queue state.
    GetQueueState {},

    /// Run all code cells from the synced document.
    /// Daemon reads cell sources from the Automerge doc and queues them.
    RunAllCells {},

    /// Send a comm message to the kernel (widget interactions).
    /// Accepts the full Jupyter message envelope to preserve header/session.
    SendComm {
        /// The full Jupyter message (header, content, buffers, etc.)
        /// Preserves frontend session/msg_id for proper widget protocol.
        message: serde_json::Value,
    },

    /// Search the kernel's input history.
    /// Returns matching history entries via HistoryResult response.
    GetHistory {
        /// Pattern to search for (glob-style, optional)
        pattern: Option<String>,
        /// Maximum number of entries to return
        n: i32,
        /// Only return unique entries (deduplicate)
        unique: bool,
    },

    /// Request code completions from the kernel.
    /// Returns matching completions via CompletionResult response.
    Complete {
        /// The code to complete
        code: String,
        /// Cursor position in the code
        cursor_pos: usize,
    },

    /// Save the notebook to disk.
    /// The daemon reads cells and metadata from the Automerge doc, merges
    /// with any existing .ipynb on disk (to preserve unknown metadata keys),
    /// and writes the result.
    ///
    /// If `path` is provided, saves to that path (with .ipynb appended if needed).
    /// If `path` is None, saves to the room's notebook_path (original file location).
    SaveNotebook {
        /// If true, format code cells before saving (e.g., with ruff).
        format_cells: bool,
        /// Optional target path. If None, uses the room's notebook_path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// Clone the notebook to a new path with fresh env_id and cleared outputs.
    /// Used for "Save As Copy" functionality - creates a new independent notebook
    /// without affecting the current document.
    CloneNotebook {
        /// Target path for the cloned notebook (absolute, .ipynb appended if needed).
        path: String,
    },

    /// Sync environment with current metadata (hot-install new packages).
    /// Only supported for UV inline deps. Falls back to restart for removals/conda.
    SyncEnvironment {},

    /// Get the full Automerge document bytes from the daemon's canonical doc.
    /// Used by the frontend to bootstrap its WASM Automerge peer.
    GetDocBytes {},

    /// Get raw metadata JSON from the daemon's Automerge doc.
    /// Returns the value stored at the given key.
    GetRawMetadata {
        /// Metadata key to read.
        key: String,
    },

    /// Set raw metadata JSON in the daemon's Automerge doc.
    /// Writes the JSON string at the given key, then syncs to all peers.
    SetRawMetadata {
        /// Metadata key to write.
        key: String,
        /// JSON string value to store.
        value: String,
    },

    /// Get the typed notebook metadata snapshot from native Automerge keys.
    /// Returns the serialized NotebookMetadataSnapshot, or None if not available.
    GetMetadataSnapshot {},

    /// Set the typed notebook metadata snapshot using native Automerge keys.
    /// Takes a serialized NotebookMetadataSnapshot JSON string.
    SetMetadataSnapshot {
        /// JSON string of NotebookMetadataSnapshot.
        snapshot: String,
    },

    /// Check if a runtime tool is available (e.g., "deno").
    /// The daemon checks without triggering bootstrap — safe for UI hints.
    CheckToolAvailable { tool: String },
}

/// Responses from daemon to notebook app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum NotebookResponse {
    /// Kernel launched successfully.
    KernelLaunched {
        kernel_type: String,
        env_source: String,
        /// Environment config used at launch (for sync detection).
        launched_config: LaunchedEnvConfig,
    },

    /// Kernel was already running (returned existing info).
    KernelAlreadyRunning {
        kernel_type: String,
        env_source: String,
        /// Environment config used at launch (for sync detection).
        launched_config: LaunchedEnvConfig,
    },

    /// Cell queued for execution.
    CellQueued {
        cell_id: String,
        execution_id: String,
    },

    /// Outputs cleared.
    OutputsCleared { cell_id: String },

    /// Interrupt sent to kernel.
    InterruptSent {},

    /// Kernel shutdown initiated.
    KernelShuttingDown {},

    /// No kernel is running.
    NoKernel {},

    /// Kernel info response.
    KernelInfo {
        kernel_type: Option<String>,
        env_source: Option<String>,
        status: String, // "idle", "busy", "not_started"
    },

    /// Queue state response.
    QueueState {
        executing: Option<QueueEntry>,
        queued: Vec<QueueEntry>,
    },

    /// All cells queued for execution.
    AllCellsQueued { queued: Vec<QueueEntry> },

    /// Notebook saved successfully to disk.
    NotebookSaved {
        /// The absolute path where the notebook was written.
        path: String,
        /// If the notebook was ephemeral (UUID-keyed) and has been re-keyed to a
        /// file-path room, this contains the new canonical notebook_id.
        /// Clients should update their local notebook_id to this value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_notebook_id: Option<String>,
    },

    /// Notebook cloned successfully to a new file.
    NotebookCloned {
        /// The absolute path where the cloned notebook was written.
        path: String,
    },

    /// Generic success.
    Ok {},

    /// Error response.
    Error { error: String },

    /// History search result.
    HistoryResult { entries: Vec<HistoryEntry> },

    /// Code completion result.
    CompletionResult {
        items: Vec<CompletionItem>,
        cursor_start: usize,
        cursor_end: usize,
    },

    /// Environment sync started (installing new packages).
    SyncEnvironmentStarted {
        /// Packages being installed
        packages: Vec<String>,
    },

    /// Environment sync completed successfully.
    SyncEnvironmentComplete {
        /// Packages that were installed
        synced_packages: Vec<String>,
    },

    /// Environment sync failed (fall back to restart).
    SyncEnvironmentFailed {
        /// Error message explaining why sync failed
        error: String,
        /// Whether the user should restart instead
        needs_restart: bool,
    },

    /// Full Automerge document bytes from the daemon's canonical doc.
    DocBytes {
        /// Raw Automerge document bytes, encoded as a Vec for JSON transport.
        bytes: Vec<u8>,
    },

    /// Raw metadata JSON value from the daemon's Automerge doc.
    RawMetadata {
        /// The metadata JSON string, or None if the key doesn't exist.
        value: Option<String>,
    },

    /// Metadata was set successfully.
    MetadataSet {},

    /// Typed notebook metadata snapshot from native Automerge keys.
    MetadataSnapshot {
        /// Serialized NotebookMetadataSnapshot JSON, or None if not available.
        snapshot: Option<String>,
    },

    /// Tool availability result.
    ToolAvailable { available: bool },
}

/// Broadcast messages from daemon to all peers in a room.
///
/// These are sent proactively when kernel events occur, not as responses
/// to specific requests. All connected windows receive these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NotebookBroadcast {
    /// Kernel status changed.
    KernelStatus {
        status: String,          // "starting", "idle", "busy", "error", "shutdown"
        cell_id: Option<String>, // which cell triggered status change
    },

    /// Execution started for a cell.
    ExecutionStarted {
        cell_id: String,
        execution_id: String,
        execution_count: i64,
    },

    /// Output produced by a cell.
    Output {
        cell_id: String,
        execution_id: String,
        output_type: String, // "stream", "display_data", "execute_result", "error"
        output_json: String, // Serialized Jupyter output content
        /// If Some, this is an update to an existing output at the given index.
        /// If None, this is a new output to append.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_index: Option<usize>,
    },

    /// Display output updated in place (update_display_data).
    DisplayUpdate {
        display_id: String,
        data: serde_json::Value,
        metadata: serde_json::Map<String, serde_json::Value>,
    },

    /// Execution completed for a cell.
    ExecutionDone {
        cell_id: String,
        execution_id: String,
    },

    /// Queue state changed.
    QueueChanged {
        executing: Option<QueueEntry>,
        queued: Vec<QueueEntry>,
    },

    /// Kernel error (failed to launch, crashed, etc.)
    KernelError { error: String },

    /// Outputs cleared for a cell.
    OutputsCleared { cell_id: String },

    /// Comm message from kernel (ipywidgets protocol).
    /// Broadcast to all connected peers so all windows can display widgets.
    Comm {
        /// Message type: "comm_open", "comm_msg", "comm_close"
        msg_type: String,
        /// Message content (comm_id, data, target_name, etc.)
        content: serde_json::Value,
        /// Binary buffers (base64-encoded when serialized to JSON)
        #[serde(default)]
        buffers: Vec<Vec<u8>>,
    },

    /// Initial comm state sync sent to newly connected clients.
    /// Contains all active comm channels so new windows can reconstruct widgets.
    CommSync {
        /// All active comm snapshots
        comms: Vec<CommSnapshot>,
    },

    /// Environment progress update during kernel launch.
    ///
    /// Carries rich progress phases (repodata, solve, download, link)
    /// from `kernel_env` so the frontend can display detailed status.
    EnvProgress {
        env_type: String,
        #[serde(flatten)]
        phase: kernel_env::EnvProgressPhase,
    },

    /// Environment sync state changed.
    ///
    /// Broadcast when notebook metadata changes and differs from the
    /// kernel's launched configuration. All connected windows can show
    /// the sync UI in response.
    EnvSyncState {
        /// Whether the current metadata matches the launched config.
        in_sync: bool,
        /// What's different (for UI display). None if in_sync is true.
        #[serde(skip_serializing_if = "Option::is_none")]
        diff: Option<EnvSyncDiff>,
    },

    /// The room was re-keyed from an ephemeral UUID to a file-path ID.
    ///
    /// Broadcast to all peers when an untitled notebook is saved so they can
    /// update their local notebook_id. Without this, peers that disconnect
    /// and reconnect would use the stale UUID and end up in a new empty room.
    RoomRenamed {
        /// The new canonical notebook_id (file path).
        new_notebook_id: String,
    },

    /// Notebook was autosaved to disk by the daemon.
    NotebookAutosaved { path: String },
}
