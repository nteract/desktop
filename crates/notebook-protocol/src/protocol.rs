//! Notebook-specific protocol types extracted from runtimed.
//!
//! Pure data definitions (structs and enums) for the notebook sync protocol.
//! No `impl` blocks — just shapes + serde derives.

use std::path::PathBuf;

use crate::connection::{EnvSource, LaunchSpec};
pub use notebook_wire::{
    InitialLoadPhaseWire, NotebookDocPhaseWire, RuntimeStatePhaseWire, SessionControlMessage,
    SessionSyncStatusWire,
};
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

/// Frontend-observed notebook state used to guard trust-approved follow-up actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuardedNotebookProvenance {
    pub observed_heads: Vec<String>,
}

/// Dependency state used to guard trust-approved sync.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyGuard {
    pub observed_heads: Vec<String>,
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
/// Formalized in protocol v3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookRequestEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Causal precondition for handling this request.
    ///
    /// When present, the daemon must not evaluate the request until its
    /// notebook document has incorporated every listed Automerge change hash.
    /// The document may have advanced beyond these heads; this is a
    /// containment check, not an equality check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_heads: Vec<String>,
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
        /// Request-time launch source: "auto", "auto:uv", "uv:inline", etc.
        env_source: LaunchSpec,
        /// Path to the notebook file (for working directory)
        notebook_path: Option<String>,
    },

    /// Execute a cell by reading its source from the automerge doc.
    ExecuteCell { cell_id: String },

    /// Execute a cell only if it still matches the frontend-observed state
    /// captured when a trust gate opened.
    ExecuteCellGuarded {
        cell_id: String,
        observed_heads: Vec<String>,
    },

    /// Interrupt the currently executing cell.
    InterruptExecution {},

    /// Shutdown the kernel for this room.
    ShutdownKernel {},

    /// Run all code cells from the synced document.
    /// Daemon reads cell sources from the Automerge doc and queues them.
    RunAllCells {},

    /// Run all code cells only if the current code-cell list still matches
    /// the frontend-observed state captured when a trust gate opened.
    RunAllCellsGuarded { observed_heads: Vec<String> },

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

    /// Fork the current notebook into a new ephemeral (in-memory only) room.
    ///
    /// Creates a new UUID, copies cells + metadata from the source room,
    /// resets env_id, clears outputs and trust. The new room exists only
    /// on the daemon until a peer connects to it via `Handshake::NotebookSync`
    /// and optionally promotes it to file-backed via Save-As.
    ///
    /// Outputs and execution counts are NOT copied — forking widget state
    /// into a kernel-less room would render disconnected live comms.
    CloneAsEphemeral {
        /// Source notebook UUID. Must refer to a room currently loaded in
        /// the daemon (file-backed, untitled, or ephemeral).
        source_notebook_id: String,
    },

    /// Sync environment with current metadata (hot-install new packages).
    ///
    /// The daemon always enforces trust before installing. `guard` is supplied
    /// when the request follows a trust dialog and must still match the
    /// dependency metadata the user approved.
    SyncEnvironment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<DependencyGuard>,
    },

    /// Approve the current dependency metadata for this notebook.
    ///
    /// The daemon signs the current runt metadata and writes the trust fields
    /// into the Automerge document. `observed_heads` is supplied when approval
    /// is tied to an already-open trust dialog; the daemon reconstructs the
    /// dependency metadata at those heads and refuses to sign if the current
    /// dependency metadata no longer matches what the user reviewed.
    ApproveTrust {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_heads: Option<Vec<String>>,
    },

    /// Approve creating/syncing a project-file environment for the current
    /// project file snapshot. This is intentionally separate from
    /// `ApproveTrust`: it records local project setup approval and does not
    /// sign notebook dependency metadata.
    ApproveProjectEnvironment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_file_path: Option<String>,
    },

    /// Get the full Automerge document bytes from the daemon's canonical doc.
    /// Used by the frontend to bootstrap its WASM Automerge peer.
    GetDocBytes {},
}

/// Responses from daemon to notebook app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum NotebookResponse {
    /// Kernel launched successfully.
    KernelLaunched {
        kernel_type: String,
        env_source: EnvSource,
        /// Environment config used at launch (for sync detection).
        launched_config: LaunchedEnvConfig,
    },

    /// Kernel was already running (returned existing info).
    KernelAlreadyRunning {
        kernel_type: String,
        env_source: EnvSource,
        /// Environment config used at launch (for sync detection).
        launched_config: LaunchedEnvConfig,
    },

    /// Cell queued for execution.
    CellQueued {
        cell_id: String,
        execution_id: String,
    },

    /// Interrupt sent to kernel.
    InterruptSent {},

    /// Kernel shutdown initiated.
    KernelShuttingDown {},

    /// No kernel is running.
    NoKernel {},

    /// A guarded action was not queued because current notebook state no
    /// longer matches the state the user approved.
    GuardRejected { reason: String },

    /// All cells queued for execution.
    AllCellsQueued { queued: Vec<QueueEntry> },

    /// Notebook saved successfully to disk.
    NotebookSaved {
        /// The absolute path where the notebook was written.
        path: String,
    },

    /// Save failed with a structured error.
    SaveError { error: SaveErrorKind },

    /// Notebook forked into a new ephemeral room.
    NotebookCloned {
        /// UUID of the newly-created ephemeral room.
        notebook_id: String,
        /// Effective working directory the cloned room inherits from its
        /// source: the source's .ipynb parent if file-backed, or the
        /// source room's explicit working_dir for untitled sources.
        /// Passed through to new-window creation for project-file resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
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
}

/// Broadcast messages from daemon to all peers in a room.
///
/// Ephemeral, room-wide events. Custom comm messages (ipywidgets model
/// updates, button clicks) are the only traffic that still flows here.
/// Kernel state, execution lifecycle, queue, outputs, the notebook's
/// `path`, `last_saved` timestamp, and environment-preparation progress
/// all live in `RuntimeStateDoc` (frame type `0x05`) — they used to flow
/// as broadcasts and the dead variants were removed once the doc became
/// authoritative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NotebookBroadcast {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPorts {
    pub stdin: u16,
    pub control: u16,
    pub hb: u16,
    pub shell: u16,
    pub iopub: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum RuntimeAgentRequest {
    /// Launch a kernel with the given configuration.
    /// Environment is already prepared by the coordinator.
    LaunchKernel {
        kernel_type: String,
        env_source: EnvSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_path: Option<String>,
        launched_config: LaunchedEnvConfig,
        kernel_ports: KernelPorts,
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
        env_source: EnvSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_path: Option<String>,
        launched_config: LaunchedEnvConfig,
        kernel_ports: KernelPorts,
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
    KernelLaunched { env_source: EnvSource },

    /// Kernel restarted successfully (same runtime agent, new kernel).
    KernelRestarted { env_source: EnvSource },

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
    use std::collections::{BTreeMap, BTreeSet};

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
            required_heads: vec!["a".repeat(64)],
            request: NotebookRequest::ExecuteCell {
                cell_id: "cell-42".into(),
            },
        };
        let json = serde_json::to_value(&env).expect("serialize");
        assert_eq!(json["id"], "req-1");
        assert_eq!(json["required_heads"][0], "a".repeat(64));
        assert_eq!(json["action"], "execute_cell");
        assert_eq!(json["cell_id"], "cell-42");
        let parsed: NotebookRequestEnvelope = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.id.as_deref(), Some("req-1"));
        assert_eq!(parsed.required_heads, vec!["a".repeat(64)]);
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

    #[test]
    fn guarded_requests_round_trip() {
        let observed_heads = vec!["0123456789abcdef".to_string()];
        let cases = vec![
            NotebookRequest::ExecuteCellGuarded {
                cell_id: "cell-1".to_string(),
                observed_heads: observed_heads.clone(),
            },
            NotebookRequest::RunAllCellsGuarded {
                observed_heads: observed_heads.clone(),
            },
            NotebookRequest::SyncEnvironment {
                guard: Some(DependencyGuard {
                    observed_heads: observed_heads.clone(),
                }),
            },
            NotebookRequest::ApproveTrust {
                observed_heads: Some(observed_heads),
            },
            NotebookRequest::ApproveProjectEnvironment {
                project_file_path: Some("/tmp/project/environment.yml".into()),
            },
        ];

        for request in cases {
            let json = serde_json::to_string(&request).expect("serialize guarded request");
            let parsed: NotebookRequest =
                serde_json::from_str(&json).expect("deserialize guarded request");
            assert_eq!(
                serde_json::to_value(parsed).expect("reserialize guarded request"),
                serde_json::to_value(request).expect("serialize original guarded request")
            );
        }
    }

    #[test]
    fn guard_rejected_response_round_trip() {
        let response = NotebookResponse::GuardRejected {
            reason: "Notebook changed before the action could run.".to_string(),
        };
        let json = serde_json::to_value(&response).expect("serialize GuardRejected");
        assert_eq!(json["result"], "guard_rejected");
        assert_eq!(
            json["reason"],
            "Notebook changed before the action could run."
        );
        let parsed: NotebookResponse =
            serde_json::from_value(json).expect("deserialize GuardRejected");
        assert_eq!(
            serde_json::to_value(parsed).expect("reserialize GuardRejected"),
            serde_json::to_value(response).expect("serialize original GuardRejected")
        );
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
                "execute_cell_guarded",
                serde_json::json!({
                    "action": "execute_cell_guarded",
                    "cell_id": "c1",
                    "observed_heads": ["abc"],
                }),
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
                "sync_environment_with_guard",
                serde_json::json!({
                    "action": "sync_environment",
                    "guard": {
                        "observed_heads": ["abc"],
                    },
                }),
            ),
            (
                "approve_trust",
                serde_json::json!({
                    "action": "approve_trust",
                    "observed_heads": ["abc"],
                }),
            ),
            (
                "approve_project_environment",
                serde_json::json!({
                    "action": "approve_project_environment",
                    "project_file_path": "/tmp/project/environment.yml",
                }),
            ),
            (
                "run_all_cells",
                serde_json::json!({ "action": "run_all_cells" }),
            ),
            (
                "run_all_cells_guarded",
                serde_json::json!({
                    "action": "run_all_cells_guarded",
                    "observed_heads": ["abc"],
                }),
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
            (
                "save_notebook",
                serde_json::json!({
                    "action": "save_notebook",
                    "format_cells": true,
                    "path": "/tmp/example.ipynb",
                }),
            ),
            (
                "clone_as_ephemeral",
                serde_json::json!({
                    "action": "clone_as_ephemeral",
                    "source_notebook_id": "source-room",
                }),
            ),
            (
                "get_doc_bytes",
                serde_json::json!({ "action": "get_doc_bytes" }),
            ),
            (
                "send_comm",
                serde_json::json!({
                    "action": "send_comm",
                    "message": {
                        "header": {},
                        "content": {},
                        "buffers": [],
                        "channel": "shell",
                    },
                }),
            ),
        ];

        let request_actions = cases
            .iter()
            .map(|(_, json)| {
                json.get("action")
                    .and_then(serde_json::Value::as_str)
                    .expect("case has action")
                    .to_owned()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            request_actions,
            expected_values(crate::typescript::NOTEBOOK_REQUEST_TYPES),
            "TypeScript request discriminator list must match the serde request shapes covered here"
        );

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

    fn extract_string_array(source: &str, const_name: &str) -> BTreeSet<String> {
        let needle = format!("export const {const_name} = [");
        let start = source
            .find(&needle)
            .unwrap_or_else(|| panic!("missing TS const {const_name}"))
            + needle.len();
        let rest = &source[start..];
        let end = rest
            .find("] as const")
            .unwrap_or_else(|| panic!("missing end of TS const {const_name}"));

        rest[..end]
            .split(',')
            .filter_map(|part| {
                let value = part.trim().trim_matches('\n').trim();
                value
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .map(ToOwned::to_owned)
            })
            .collect()
    }

    fn extract_frame_type_object(source: &str) -> BTreeMap<String, u8> {
        let needle = "export const FrameType = {";
        let start = source.find(needle).expect("missing TS FrameType object") + needle.len();
        let rest = &source[start..];
        let end = rest
            .find("} as const")
            .expect("missing end of TS FrameType");

        rest[..end]
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with("/**") || line.starts_with('*') {
                    return None;
                }
                let (name, value) = line.trim_end_matches(',').split_once(':')?;
                let value = value.trim();
                let parsed = if let Some(hex) = value.strip_prefix("0x") {
                    u8::from_str_radix(hex, 16).expect("valid hex frame byte")
                } else {
                    value.parse::<u8>().expect("valid decimal frame byte")
                };
                Some((name.trim().to_owned(), parsed))
            })
            .collect()
    }

    fn expected_values(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn typescript_protocol_contract_matches_rust_wire_discriminants() {
        let ts_contract = include_str!("../../../packages/runtimed/src/protocol-contract.ts");
        let ts_transport = include_str!("../../../packages/runtimed/src/transport.ts");

        assert_eq!(
            extract_string_array(ts_contract, "NOTEBOOK_REQUEST_TYPES"),
            expected_values(crate::typescript::NOTEBOOK_REQUEST_TYPES)
        );
        assert_eq!(
            extract_string_array(ts_contract, "NOTEBOOK_RESPONSE_RESULTS"),
            expected_values(crate::typescript::NOTEBOOK_RESPONSE_RESULTS)
        );
        assert_eq!(
            extract_string_array(ts_contract, "SESSION_CONTROL_TYPES"),
            expected_values(crate::typescript::SESSION_CONTROL_TYPES)
        );
        assert_eq!(
            extract_string_array(ts_contract, "NOTEBOOK_DOC_PHASES"),
            expected_values(crate::typescript::NOTEBOOK_DOC_PHASES)
        );
        assert_eq!(
            extract_string_array(ts_contract, "RUNTIME_STATE_PHASES"),
            expected_values(crate::typescript::RUNTIME_STATE_PHASES)
        );
        assert_eq!(
            extract_string_array(ts_contract, "INITIAL_LOAD_PHASES"),
            expected_values(crate::typescript::INITIAL_LOAD_PHASES)
        );

        assert_eq!(
            extract_frame_type_object(ts_transport),
            BTreeMap::from([
                (
                    "AUTOMERGE_SYNC".to_string(),
                    notebook_wire::frame_types::AUTOMERGE_SYNC
                ),
                ("REQUEST".to_string(), notebook_wire::frame_types::REQUEST),
                ("RESPONSE".to_string(), notebook_wire::frame_types::RESPONSE),
                (
                    "BROADCAST".to_string(),
                    notebook_wire::frame_types::BROADCAST
                ),
                ("PRESENCE".to_string(), notebook_wire::frame_types::PRESENCE),
                (
                    "RUNTIME_STATE_SYNC".to_string(),
                    notebook_wire::frame_types::RUNTIME_STATE_SYNC
                ),
                (
                    "POOL_STATE_SYNC".to_string(),
                    notebook_wire::frame_types::POOL_STATE_SYNC
                ),
                (
                    "SESSION_CONTROL".to_string(),
                    notebook_wire::frame_types::SESSION_CONTROL
                ),
            ])
        );
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
    fn runtime_agent_launch_and_restart_include_kernel_ports() {
        let ports = KernelPorts {
            stdin: 9000,
            control: 9001,
            hb: 9002,
            shell: 9003,
            iopub: 9004,
        };
        let launch = RuntimeAgentRequest::LaunchKernel {
            kernel_type: "python".into(),
            env_source: EnvSource::Prewarmed(crate::connection::PackageManager::Uv),
            notebook_path: None,
            launched_config: Default::default(),
            kernel_ports: ports,
            env_vars: Default::default(),
        };
        let json = serde_json::to_value(&launch).unwrap();
        assert_eq!(json["kernel_ports"]["stdin"], 9000);
        assert_eq!(json["kernel_ports"]["control"], 9001);
        assert_eq!(json["kernel_ports"]["hb"], 9002);
        assert_eq!(json["kernel_ports"]["shell"], 9003);
        assert_eq!(json["kernel_ports"]["iopub"], 9004);

        let parsed: RuntimeAgentRequest = serde_json::from_value(json).unwrap();
        assert!(matches!(
            parsed,
            RuntimeAgentRequest::LaunchKernel {
                kernel_ports,
                ..
            } if kernel_ports == ports
        ));

        let restart = RuntimeAgentRequest::RestartKernel {
            kernel_type: "python".into(),
            env_source: EnvSource::Inline(crate::connection::PackageManager::Conda),
            notebook_path: None,
            launched_config: Default::default(),
            kernel_ports: ports,
            env_vars: Default::default(),
        };
        let json = serde_json::to_value(&restart).unwrap();
        let parsed: RuntimeAgentRequest = serde_json::from_value(json).unwrap();
        assert!(matches!(
            parsed,
            RuntimeAgentRequest::RestartKernel {
                kernel_ports,
                ..
            } if kernel_ports == ports
        ));
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
            env_source: EnvSource::Prewarmed(crate::connection::PackageManager::Uv),
            notebook_path: None,
            launched_config: Default::default(),
            kernel_ports: KernelPorts {
                stdin: 9000,
                control: 9001,
                hb: 9002,
                shell: 9003,
                iopub: 9004,
            },
            env_vars: Default::default(),
        }
        .is_command());
        assert!(!RuntimeAgentRequest::RestartKernel {
            kernel_type: "python".into(),
            env_source: EnvSource::Inline(crate::connection::PackageManager::Conda),
            notebook_path: None,
            launched_config: Default::default(),
            kernel_ports: KernelPorts {
                stdin: 9005,
                control: 9006,
                hb: 9007,
                shell: 9008,
                iopub: 9009,
            },
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
