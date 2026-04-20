//! Notebook-specific protocol types extracted from runtimed.
//!
//! Pure data definitions (structs and enums) for the notebook sync protocol.
//! No `impl` blocks — just shapes + serde derives.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Data structs referenced by protocol enums ───────────────────────────────

/// Optional runtime behaviors toggled by the user.
///
/// Single source of truth for feature-flag state across the daemon,
/// settings UI, and kernel launch pipeline. Each flag defaults to `false`,
/// so adding a new flag is one extra field here, no new arguments threaded
/// through call sites.
///
/// Serialized flat via `#[serde(flatten)]` in parent structs, so the
/// on-wire JSON and Automerge keys stay at the top level (`bootstrap_dx`)
/// for backward compatibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Install `nteract-kernel-launcher` and `dx` into UV kernels, launch
    /// via `nteract_kernel_launcher`, and register dx display formatters
    /// before the first user cell.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bootstrap_dx: bool,
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

    /// Pixi inline deps — conda matchspecs (if env_source is "pixi:inline")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixi_deps: Option<Vec<String>>,

    /// Pixi project deps snapshot for drift detection (pixi:toml only).
    /// Combined sorted list of conda + pypi dependency names from pixi.toml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixi_toml_deps: Option<Vec<String>>,

    /// Path to the pixi.toml or pyproject.toml with [tool.pixi] (pixi:toml only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixi_toml_path: Option<PathBuf>,

    /// Path to pyproject.toml (uv:pyproject only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pyproject_path: Option<PathBuf>,

    /// Path to environment.yml (conda:env_yml only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_yml_path: Option<PathBuf>,

    /// Conda deps snapshot from environment.yml for drift detection (conda:env_yml only).
    /// Combined sorted list of conda dependency names from environment.yml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_yml_deps: Option<Vec<String>>,

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

    /// Snapshot of the user's feature-flag settings at launch time.
    /// Flattened on the wire so each flag sits at the top level (e.g.
    /// `bootstrap_dx`) for backward compatibility.
    #[serde(default, flatten)]
    pub feature_flags: FeatureFlags,

    /// Packages pre-installed in the prewarmed environment (empty for inline envs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prewarmed_packages: Vec<String>,
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

/// An entry in the execution queue, pairing a cell with its execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueEntry {
    pub cell_id: String,
    pub execution_id: String,
}

/// Typed environment kind for sync operations.
///
/// Replaces string-based env_type ("uv", "conda") with a discriminated union
/// that carries environment-specific data. Makes illegal states unrepresentable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "env_kind", rename_all = "snake_case")]
pub enum EnvKind {
    /// UV environment (inline or prewarmed).
    Uv { packages: Vec<String> },
    /// Conda environment (inline or prewarmed).
    Conda {
        packages: Vec<String>,
        channels: Vec<String>,
    },
}

impl EnvKind {
    /// The packages to install, regardless of environment type.
    pub fn packages(&self) -> &[String] {
        match self {
            EnvKind::Uv { packages } | EnvKind::Conda { packages, .. } => packages,
        }
    }
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

/// Structured error kinds returned in `NotebookResponse::SaveError`.
///
/// Note: `path` fields carry the serialized path string. Callers that build
/// `PathAlreadyOpen` from a `PathBuf` should use `p.to_string_lossy().into_owned()`
/// so non-UTF-8 paths degrade gracefully on the wire (Task 6.2 concern).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SaveErrorKind {
    /// Another room is currently serving this path. The agent must close
    /// the conflicting session (by UUID) before saving here.
    PathAlreadyOpen {
        /// UUID of the room that currently holds this path.
        uuid: String,
        /// The conflicting path (lossy-UTF-8 serialized from `PathBuf`).
        path: String,
    },
    /// I/O or serialization failure. Message is human-readable.
    Io { message: String },
}

/// Envelope around a `NotebookRequest` carrying a correlation id.
///
/// The id is echoed on the matching `NotebookResponseEnvelope` so the relay
/// (or any future direct JS sender) can match responses to in-flight
/// requests. Absent id == notification / fire-and-forget — not currently
/// used but kept as a free escape hatch.
///
/// Wire shape is flattened: `{"id":"...","action":"execute_cell","cell_id":"..."}`.
/// Introduced at `PROTOCOL_VERSION = 3`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookRequestEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub request: NotebookRequest,
}

/// Envelope around a `NotebookResponse` echoing the originating request id.
///
/// `id == None` is valid for out-of-band server-pushed responses that
/// don't correspond to a specific client request (not currently emitted;
/// reserved for future use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookResponseEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub response: NotebookResponse,
}

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

    /// Execute a cell by reading its source from the automerge doc.
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
    /// If `path` is None, saves to the room's current path; untitled rooms
    /// (no path) return an error.
    SaveNotebook {
        /// If true, format code cells before saving (e.g., with ruff).
        format_cells: bool,
        /// Optional target path. If None, uses the room's current path.
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
    },

    /// Save failed with a structured error.
    SaveError { error: SaveErrorKind },

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
#[allow(clippy::large_enum_variant)]
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

    /// Sent when the room's `.ipynb` path changes (untitled→saved, save-as rename).
    ///
    /// Peers update local bookkeeping but do NOT reconnect — the UUID is stable.
    /// `path` is `None` for an explicit "close file" rename (rare/future).
    ///
    /// Callers building this from a `PathBuf` should use
    /// `p.to_string_lossy().into_owned()` so non-UTF-8 paths degrade gracefully
    /// on the wire.
    PathChanged { path: Option<String> },

    /// Notebook was autosaved to disk by the daemon.
    NotebookAutosaved { path: String },

    /// Eager RuntimeState snapshot sent during connection setup.
    ///
    /// Bypasses the Automerge sync handshake so the client has kernel
    /// status immediately (prevents "not_started" → "idle" jump).
    RuntimeStateSnapshot {
        state: notebook_doc::runtime_state::RuntimeState,
    },
}

// ── Runtime agent protocol types ──────────────────────────────────────────
//
// These types define the coordinator↔runtime-agent wire contract for
// process-isolated runtime agents (#1333). The runtime agent subprocess
// communicates over stdin/stdout using the same framed protocol (frame
// types 0x01/0x02/0x03 for JSON, 0x05 for RuntimeStateDoc sync).

/// Requests from coordinator to runtime agent (frame type 0x01).
///
/// The coordinator mediates between frontend requests and the runtime agent.
/// Environment preparation happens in the coordinator; the runtime agent
/// receives a ready-to-launch configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum RuntimeAgentRequest {
    /// Launch a kernel with the given configuration.
    /// Environment is already prepared by the coordinator.
    LaunchKernel {
        kernel_type: String,
        env_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_path: Option<String>,
        launched_config: LaunchedEnvConfig,
        /// Environment variables to set for the kernel process.
        #[serde(default)]
        env_vars: std::collections::HashMap<String, String>,
    },

    /// Interrupt the currently executing cell.
    InterruptExecution,

    /// Shutdown the kernel. The runtime agent process stays alive for potential restart.
    ShutdownKernel,

    /// Restart the kernel: shut down the current kernel, create a new one,
    /// re-launch. Same runtime agent process, same socket connection. The
    /// coordinator sends this instead of spawning a new runtime agent when
    /// one is already connected.
    RestartKernel {
        kernel_type: String,
        env_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_path: Option<String>,
        launched_config: LaunchedEnvConfig,
        #[serde(default)]
        env_vars: std::collections::HashMap<String, String>,
    },

    /// Send a comm message to the kernel (widget interactions).
    SendComm { message: serde_json::Value },

    /// Request code completions from the kernel.
    Complete { code: String, cursor_pos: usize },

    /// Search the kernel's input history.
    GetHistory {
        pattern: Option<String>,
        n: i32,
        unique: bool,
    },

    /// Hot-install packages into the running kernel's environment.
    /// Supported for UV and Conda inline dependencies (additions only).
    SyncEnvironment(EnvKind),
}

/// Responses from runtime agent to coordinator (frame type 0x02).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RuntimeAgentResponse {
    /// Kernel launched successfully.
    KernelLaunched { env_source: String },

    /// Kernel restarted successfully (same runtime agent, new kernel).
    KernelRestarted { env_source: String },

    /// Code completion result.
    CompletionResult {
        items: Vec<CompletionItem>,
        cursor_start: usize,
        cursor_end: usize,
    },

    /// History search result.
    HistoryResult { entries: Vec<HistoryEntry> },

    /// Interrupt acknowledged. Contains the list of cleared queue entries.
    InterruptAcknowledged { cleared: Vec<QueueEntry> },

    /// Generic success.
    Ok,

    /// Packages installed successfully into the running env.
    EnvironmentSynced { synced_packages: Vec<String> },

    /// Error response.
    Error { error: String },
}

/// Envelope around a `RuntimeAgentRequest` carrying a correlation ID.
///
/// Every request gets a unique ID. For query RPCs (Complete, GetHistory),
/// the agent echoes the ID on the response envelope. For command RPCs
/// (fire-and-forget), the agent processes the request but sends no response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAgentRequestEnvelope {
    pub id: String,
    #[serde(flatten)]
    pub request: RuntimeAgentRequest,
}

/// Envelope around a `RuntimeAgentResponse` echoing the correlation ID.
///
/// Only used for query RPCs (Complete, GetHistory) that need sync responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAgentResponseEnvelope {
    pub id: String,
    #[serde(flatten)]
    pub response: RuntimeAgentResponse,
}

impl RuntimeAgentRequest {
    /// Returns true if this request is a command (fire-and-forget).
    /// Commands don't get a response — state flows back via CRDT.
    ///
    /// Currently: Interrupt, SendComm. Shutdown is a sync query because
    /// the daemon must confirm the kernel is dead before LaunchKernel
    /// sends RestartKernel — otherwise CRDT-queued cells can race onto
    /// the dying kernel.
    pub fn is_command(&self) -> bool {
        matches!(
            self,
            RuntimeAgentRequest::InterruptExecution | RuntimeAgentRequest::SendComm { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_kind_uv_round_trip() {
        let kind = EnvKind::Uv {
            packages: vec!["numpy".into(), "pandas".into()],
        };
        let json = serde_json::to_string(&kind).expect("failed to serialize EnvKind::Uv");
        let parsed: EnvKind = serde_json::from_str(&json).expect("failed to parse EnvKind::Uv");
        assert_eq!(kind, parsed);
    }

    #[test]
    fn env_kind_conda_round_trip() {
        let kind = EnvKind::Conda {
            packages: vec!["scipy".into()],
            channels: vec!["conda-forge".into()],
        };
        let json = serde_json::to_string(&kind).expect("failed to serialize EnvKind::Conda");
        assert!(json.contains("\"env_kind\":\"conda\""));
        let parsed: EnvKind = serde_json::from_str(&json).expect("failed to parse EnvKind::Conda");
        assert_eq!(kind, parsed);
    }

    #[test]
    fn sync_environment_request_round_trip() {
        let req = RuntimeAgentRequest::SyncEnvironment(EnvKind::Conda {
            packages: vec!["numpy".into()],
            channels: vec!["conda-forge".into(), "bioconda".into()],
        });
        let json = serde_json::to_string(&req).expect("failed to serialize SyncEnvironment");
        let parsed: RuntimeAgentRequest =
            serde_json::from_str(&json).expect("failed to parse SyncEnvironment");
        match &parsed {
            RuntimeAgentRequest::SyncEnvironment(kind) => {
                assert_eq!(kind.packages(), &["numpy".to_string()]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn env_kind_packages_returns_inner_uv_slice() {
        // Both variants share a packages() accessor — make sure each one
        // returns its own packages, not e.g. an empty slice or the wrong
        // variant's data.
        let uv = EnvKind::Uv {
            packages: vec!["pandas".into(), "polars".into()],
        };
        assert_eq!(uv.packages(), &["pandas".to_string(), "polars".to_string()]);
    }

    #[test]
    fn env_kind_packages_returns_inner_conda_slice() {
        let conda = EnvKind::Conda {
            packages: vec!["scipy".into(), "numpy".into()],
            channels: vec!["conda-forge".into()],
        };
        assert_eq!(
            conda.packages(),
            &["scipy".to_string(), "numpy".to_string()]
        );
    }

    #[test]
    fn env_kind_uv_packages_can_be_empty() {
        // Empty package list is a real case (e.g. a prewarmed env that
        // is later reused with no extra installs). packages() must
        // return an empty slice without panicking.
        let uv = EnvKind::Uv { packages: vec![] };
        assert!(uv.packages().is_empty());
    }

    #[test]
    fn deno_launched_config_serde_default_keeps_flexible_npm_imports_on() {
        // The real guarantee: a JSON object missing `flexible_npm_imports`
        // must deserialize to true. If a future refactor accidentally flips
        // the serde default to false, every legacy notebook that doesn't
        // carry the field would silently lose `npm:` autoinstall on kernel
        // restore. (Note: `derive(Default)` gives false because bool's
        // Default is false; the serde default is what fires during
        // deserialization, and that's the load-bearing path.)
        let parsed: DenoLaunchedConfig =
            serde_json::from_str("{}").expect("DenoLaunchedConfig deserializes from {{}}");
        assert!(parsed.flexible_npm_imports);
    }

    #[test]
    fn deno_launched_config_round_trip_preserves_all_fields() {
        let cfg = DenoLaunchedConfig {
            permissions: vec!["--allow-net".into(), "--allow-read=./data".into()],
            import_map: Some("./import_map.json".into()),
            config: Some("./deno.json".into()),
            flexible_npm_imports: false,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let parsed: DenoLaunchedConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn queue_entry_round_trip_preserves_ids() {
        let entry = QueueEntry {
            cell_id: "cell-abc".to_string(),
            execution_id: "exec-123-uuid".to_string(),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: QueueEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, parsed);
    }

    #[test]
    fn request_envelope_flattens_and_round_trips() {
        let env = NotebookRequestEnvelope {
            id: Some("req-1".into()),
            request: NotebookRequest::ExecuteCell {
                cell_id: "cell-42".into(),
            },
        };
        let json = serde_json::to_value(&env).expect("serialize");
        assert_eq!(json["id"], "req-1");
        assert_eq!(json["action"], "execute_cell");
        assert_eq!(json["cell_id"], "cell-42");
        let parsed: NotebookRequestEnvelope = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.id.as_deref(), Some("req-1"));
        assert!(matches!(
            parsed.request,
            NotebookRequest::ExecuteCell { cell_id } if cell_id == "cell-42"
        ));
    }

    #[test]
    fn response_envelope_flattens_and_round_trips() {
        let env = NotebookResponseEnvelope {
            id: Some("req-1".into()),
            response: NotebookResponse::CellQueued {
                cell_id: "cell-42".into(),
                execution_id: "exec-abc".into(),
            },
        };
        let json = serde_json::to_value(&env).expect("serialize");
        assert_eq!(json["id"], "req-1");
        assert_eq!(json["result"], "cell_queued");
        assert_eq!(json["cell_id"], "cell-42");
        let parsed: NotebookResponseEnvelope = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.id.as_deref(), Some("req-1"));
    }

    /// Locks in the exact JSON wire shape that the TypeScript frontend
    /// emits for each `NotebookRequest` variant. The TS `NotebookRequest`
    /// union's discriminator field names must match the Rust variant's
    /// snake_cased name — the frame-based `TauriTransport.sendRequest`
    /// translates TS's `type:` into the envelope's `action:` key verbatim,
    /// so a mismatched name produces an unparseable request on the
    /// daemon side (silent wire error + failed interrupt / execute / etc.).
    ///
    /// Adding a new variant on the Rust side? Extend this test with the
    /// JSON shape the TS caller sends. Renaming a Rust variant? This
    /// test will flag the mismatch.
    #[test]
    fn wire_action_names_match_ts_discriminators() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "launch_kernel",
                serde_json::json!({
                    "action": "launch_kernel",
                    "kernel_type": "python",
                    "env_source": "uv:inline",
                }),
            ),
            (
                "execute_cell",
                serde_json::json!({ "action": "execute_cell", "cell_id": "c1" }),
            ),
            (
                "clear_outputs",
                serde_json::json!({ "action": "clear_outputs", "cell_id": "c1" }),
            ),
            (
                "interrupt_execution",
                serde_json::json!({ "action": "interrupt_execution" }),
            ),
            (
                "shutdown_kernel",
                serde_json::json!({ "action": "shutdown_kernel" }),
            ),
            (
                "sync_environment",
                serde_json::json!({ "action": "sync_environment" }),
            ),
            (
                "run_all_cells",
                serde_json::json!({ "action": "run_all_cells" }),
            ),
            (
                "get_history",
                serde_json::json!({
                    "action": "get_history",
                    "pattern": null,
                    "n": 100,
                    "unique": false,
                }),
            ),
            (
                "complete",
                serde_json::json!({
                    "action": "complete",
                    "code": "import os\nos.",
                    "cursor_pos": 13,
                }),
            ),
        ];

        for (name, json) in cases {
            // Also validate the envelope form (with an id), which is what
            // the frame-based path actually sends.
            let mut with_id = json.clone();
            if let serde_json::Value::Object(ref mut map) = with_id {
                map.insert("id".into(), serde_json::Value::String("req-1".into()));
            }
            let _: NotebookRequestEnvelope = serde_json::from_value(with_id.clone())
                .unwrap_or_else(|e| {
                    panic!("envelope with action {:?} failed to deserialize: {e}", name)
                });
            let _: NotebookRequest = serde_json::from_value(json).unwrap_or_else(|e| {
                panic!(
                    "bare request with action {:?} failed to deserialize: {e}",
                    name
                )
            });
        }
    }

    #[test]
    fn runtime_agent_request_envelope_round_trip() {
        let envelope = RuntimeAgentRequestEnvelope {
            id: "req-42".to_string(),
            request: RuntimeAgentRequest::InterruptExecution,
        };
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["id"], "req-42");
        assert_eq!(json["action"], "interrupt_execution");

        let parsed: RuntimeAgentRequestEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.id, "req-42");
        assert!(matches!(
            parsed.request,
            RuntimeAgentRequest::InterruptExecution
        ));
    }

    #[test]
    fn runtime_agent_response_envelope_round_trip() {
        let envelope = RuntimeAgentResponseEnvelope {
            id: "req-42".to_string(),
            response: RuntimeAgentResponse::CompletionResult {
                items: vec![],
                cursor_start: 0,
                cursor_end: 5,
            },
        };
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["id"], "req-42");
        assert_eq!(json["result"], "completion_result");

        let parsed: RuntimeAgentResponseEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.id, "req-42");
    }

    #[test]
    fn runtime_agent_is_command() {
        assert!(RuntimeAgentRequest::InterruptExecution.is_command());
        assert!(!RuntimeAgentRequest::ShutdownKernel.is_command());
        assert!(!RuntimeAgentRequest::Complete {
            code: String::new(),
            cursor_pos: 0,
        }
        .is_command());
        assert!(!RuntimeAgentRequest::GetHistory {
            pattern: None,
            n: 10,
            unique: false,
        }
        .is_command());
    }

    #[test]
    fn is_command_exhaustive_classification() {
        // Fire-and-forget commands (no response needed)
        assert!(RuntimeAgentRequest::InterruptExecution.is_command());
        assert!(RuntimeAgentRequest::SendComm {
            message: serde_json::json!({})
        }
        .is_command());

        // Sync queries (response required)
        assert!(!RuntimeAgentRequest::ShutdownKernel.is_command());
        assert!(!RuntimeAgentRequest::LaunchKernel {
            kernel_type: "python".into(),
            env_source: "uv:prewarmed".into(),
            notebook_path: None,
            launched_config: Default::default(),
            env_vars: Default::default(),
        }
        .is_command());
        assert!(!RuntimeAgentRequest::RestartKernel {
            kernel_type: "python".into(),
            env_source: "conda:inline".into(),
            notebook_path: None,
            launched_config: Default::default(),
            env_vars: Default::default(),
        }
        .is_command());
        assert!(!RuntimeAgentRequest::SyncEnvironment(EnvKind::Uv {
            packages: vec!["numpy".into()]
        })
        .is_command());
    }

    #[test]
    fn shutdown_is_sync_prevents_crdt_race() {
        // ShutdownKernel MUST be a sync query. If it were fire-and-forget,
        // the daemon would return immediately and LaunchKernel could set
        // kernel_status="starting" before the agent processes shutdown.
        // Cells queued during env prep would then execute on the dying kernel.
        assert!(
            !RuntimeAgentRequest::ShutdownKernel.is_command(),
            "ShutdownKernel must be sync to prevent CRDT race with LaunchKernel"
        );
    }

    #[test]
    fn correlation_id_preserved_in_envelope_roundtrip() {
        let id = "corr-abc-123";
        let envelope = RuntimeAgentRequestEnvelope {
            id: id.to_string(),
            request: RuntimeAgentRequest::Complete {
                code: "import pa".into(),
                cursor_pos: 9,
            },
        };
        let json = serde_json::to_value(&envelope).unwrap();
        let parsed: RuntimeAgentRequestEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.id, id);

        let resp_envelope = RuntimeAgentResponseEnvelope {
            id: id.to_string(),
            response: RuntimeAgentResponse::CompletionResult {
                items: vec![],
                cursor_start: 7,
                cursor_end: 9,
            },
        };
        let resp_json = serde_json::to_value(&resp_envelope).unwrap();
        let parsed_resp: RuntimeAgentResponseEnvelope = serde_json::from_value(resp_json).unwrap();
        assert_eq!(parsed_resp.id, id);
    }
}
