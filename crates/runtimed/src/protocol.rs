//! IPC protocol types for pool daemon communication.
//!
//! Request and Response enums are serialized as JSON and sent over
//! length-prefixed frames (see `connection.rs`).

use serde::{Deserialize, Serialize};

use crate::comm_state::CommSnapshot;
use crate::{EnvType, PoolError, PoolStats, PooledEnv};

/// Requests that clients can send to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Request an environment from the pool.
    /// If available, the daemon will claim it and return the path.
    Take { env_type: EnvType },

    /// Return an environment to the pool (optional - daemon reclaims on death).
    Return { env: PooledEnv },

    /// Get current pool statistics.
    Status,

    /// Ping to check if daemon is alive.
    Ping,

    /// Request daemon shutdown (for clean termination).
    Shutdown,

    /// Flush all pooled environments and rebuild with current settings.
    FlushPool,

    /// Inspect the Automerge state for a notebook.
    InspectNotebook {
        /// The notebook ID (file path used as identifier).
        notebook_id: String,
    },

    /// List all active notebook rooms.
    ListRooms,
}

/// Responses from the daemon to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Successfully took an environment.
    Env { env: PooledEnv },

    /// No environment available right now.
    Empty,

    /// Environment returned successfully.
    Returned,

    /// Pool statistics.
    Stats { stats: PoolStats },

    /// Pong response to ping.
    Pong,

    /// Shutdown acknowledged.
    ShuttingDown,

    /// Pool flush acknowledged — environments will be rebuilt.
    Flushed,

    /// An error occurred.
    Error { message: String },

    /// Notebook state inspection result.
    NotebookState {
        /// The notebook ID.
        notebook_id: String,
        /// Cell snapshots from the Automerge doc.
        cells: Vec<crate::notebook_doc::CellSnapshot>,
        /// Whether this was loaded from a live room or from disk.
        source: String,
        /// Kernel info if a kernel is running.
        kernel_info: Option<NotebookKernelInfo>,
    },

    /// List of active notebook rooms.
    RoomsList { rooms: Vec<RoomInfo> },
}

/// Kernel info for a notebook room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookKernelInfo {
    pub kernel_type: String,
    pub env_source: String,
    pub status: String,
}

/// Info about an active notebook room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub notebook_id: String,
    pub active_peers: usize,
    pub has_kernel: bool,
    /// Kernel type if running (e.g., "python", "deno")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_type: Option<String>,
    /// Environment source if kernel is running (e.g., "uv:inline", "conda:prewarmed")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_source: Option<String>,
    /// Kernel status if running (e.g., "idle", "busy", "starting")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_status: Option<String>,
}

/// Blob channel request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BlobRequest {
    /// Store a blob. The next frame is the raw binary data.
    Store { media_type: String },
    /// Query the blob HTTP server port.
    GetPort,
}

/// Blob channel response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlobResponse {
    /// Blob stored successfully.
    Stored { hash: String },
    /// Blob server port.
    Port { port: u16 },
    /// Error.
    Error { error: String },
}

// =============================================================================
// Notebook Sync Protocol (Phase 8: Daemon-owned kernel execution)
// =============================================================================

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
    QueueCell { cell_id: String, code: String },

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

    // ── Dependency Management ──────────────────────────────────────
    /// Get dependencies from the Automerge document.
    GetDependencies {},

    /// Set UV dependencies in the Automerge document.
    SetUvDependencies {
        dependencies: Vec<String>,
        requires_python: Option<String>,
    },

    /// Set Conda dependencies in the Automerge document.
    SetCondaDependencies {
        dependencies: Vec<String>,
        channels: Vec<String>,
        python: Option<String>,
    },

    /// Add a single UV dependency.
    AddUvDependency { package: String },

    /// Remove a UV dependency.
    RemoveUvDependency { package: String },

    /// Add a single Conda dependency.
    AddCondaDependency { package: String },

    /// Remove a Conda dependency.
    RemoveCondaDependency { package: String },
}

/// Responses from daemon to notebook app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum NotebookResponse {
    /// Kernel launched successfully.
    KernelLaunched {
        kernel_type: String,
        env_source: String,
    },

    /// Kernel was already running (returned existing info).
    KernelAlreadyRunning {
        kernel_type: String,
        env_source: String,
    },

    /// Cell queued for execution.
    CellQueued { cell_id: String },

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
        executing: Option<String>, // cell_id currently executing
        queued: Vec<String>,       // cell_ids waiting
    },

    /// All cells queued for execution.
    AllCellsQueued {
        count: usize, // number of code cells queued
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

    // ── Dependency Responses ───────────────────────────────────────
    /// Dependencies from the Automerge document.
    Dependencies {
        uv: Option<UvDependenciesJson>,
        conda: Option<CondaDependenciesJson>,
    },

    /// Dependency operation completed successfully.
    DependencyUpdated {},
}

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

/// UV dependencies for JSON serialization in protocol messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UvDependenciesJson {
    /// List of PEP 508 dependency specifiers
    pub dependencies: Vec<String>,
    /// Python version requirement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_python: Option<String>,
}

/// Conda dependencies for JSON serialization in protocol messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CondaDependenciesJson {
    /// List of conda package specs
    pub dependencies: Vec<String>,
    /// List of conda channels
    pub channels: Vec<String>,
    /// Python version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
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
        execution_count: i64,
    },

    /// Output produced by a cell.
    Output {
        cell_id: String,
        output_type: String, // "stream", "display_data", "execute_result", "error"
        output_json: String, // Serialized Jupyter output content
    },

    /// Display output updated in place (update_display_data).
    DisplayUpdate {
        display_id: String,
        data: serde_json::Value,
        metadata: serde_json::Map<String, serde_json::Value>,
    },

    /// Execution completed for a cell.
    ExecutionDone { cell_id: String },

    /// Queue state changed.
    QueueChanged {
        executing: Option<String>,
        queued: Vec<String>,
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

    /// Dependencies changed in the Automerge document.
    /// Broadcast when UV or Conda dependencies are added, removed, or modified.
    DepsChanged {
        uv: Option<UvDependenciesJson>,
        conda: Option<CondaDependenciesJson>,
    },
}

// =============================================================================
// Daemon Broadcast Protocol (Global daemon state)
// =============================================================================

/// Broadcast messages for global daemon state (not per-notebook).
///
/// These are sent to all connected clients when daemon-wide state changes,
/// such as pool errors due to invalid default packages in settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonBroadcast {
    /// Pool state changed (error occurred, error cleared, etc.)
    PoolState {
        /// Error info for UV pool (None if healthy).
        uv_error: Option<PoolError>,
        /// Error info for Conda pool (None if healthy).
        conda_error: Option<PoolError>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn roundtrip_request(req: &Request) -> Request {
        let bytes = serde_json::to_vec(req).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn roundtrip_response(resp: &Response) -> Response {
        let bytes = serde_json::to_vec(resp).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn test_request_take_uv() {
        let req = Request::Take {
            env_type: EnvType::Uv,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("take"));
        assert!(json.contains("uv"));

        match roundtrip_request(&req) {
            Request::Take { env_type } => assert_eq!(env_type, EnvType::Uv),
            _ => panic!("unexpected request type"),
        }
    }

    #[test]
    fn test_request_take_conda() {
        let req = Request::Take {
            env_type: EnvType::Conda,
        };
        match roundtrip_request(&req) {
            Request::Take { env_type } => assert_eq!(env_type, EnvType::Conda),
            _ => panic!("unexpected request type"),
        }
    }

    #[test]
    fn test_request_return() {
        let env = PooledEnv {
            env_type: EnvType::Uv,
            venv_path: PathBuf::from("/tmp/test-venv"),
            python_path: PathBuf::from("/tmp/test-venv/bin/python"),
        };
        let req = Request::Return { env: env.clone() };
        match roundtrip_request(&req) {
            Request::Return { env: parsed_env } => {
                assert_eq!(parsed_env.venv_path, env.venv_path);
                assert_eq!(parsed_env.python_path, env.python_path);
            }
            _ => panic!("unexpected request type"),
        }
    }

    #[test]
    fn test_request_status() {
        assert!(matches!(
            roundtrip_request(&Request::Status),
            Request::Status
        ));
    }

    #[test]
    fn test_request_ping() {
        assert!(matches!(roundtrip_request(&Request::Ping), Request::Ping));
    }

    #[test]
    fn test_request_shutdown() {
        assert!(matches!(
            roundtrip_request(&Request::Shutdown),
            Request::Shutdown
        ));
    }

    #[test]
    fn test_request_flush_pool() {
        assert!(matches!(
            roundtrip_request(&Request::FlushPool),
            Request::FlushPool
        ));
    }

    #[test]
    fn test_response_env() {
        let env = PooledEnv {
            env_type: EnvType::Uv,
            venv_path: PathBuf::from("/tmp/test-venv"),
            python_path: PathBuf::from("/tmp/test-venv/bin/python"),
        };
        let resp = Response::Env { env: env.clone() };
        match roundtrip_response(&resp) {
            Response::Env { env: parsed_env } => {
                assert_eq!(parsed_env.venv_path, env.venv_path);
            }
            _ => panic!("unexpected response type"),
        }
    }

    #[test]
    fn test_response_empty() {
        assert!(matches!(
            roundtrip_response(&Response::Empty),
            Response::Empty
        ));
    }

    #[test]
    fn test_response_returned() {
        assert!(matches!(
            roundtrip_response(&Response::Returned),
            Response::Returned
        ));
    }

    #[test]
    fn test_response_stats() {
        let stats = PoolStats {
            uv_available: 3,
            uv_warming: 1,
            conda_available: 2,
            conda_warming: 0,
            uv_error: None,
            conda_error: None,
        };
        let resp = Response::Stats {
            stats: stats.clone(),
        };
        match roundtrip_response(&resp) {
            Response::Stats { stats: s } => {
                assert_eq!(s.uv_available, 3);
                assert_eq!(s.uv_warming, 1);
                assert_eq!(s.conda_available, 2);
                assert_eq!(s.conda_warming, 0);
            }
            _ => panic!("unexpected response type"),
        }
    }

    #[test]
    fn test_response_pong() {
        assert!(matches!(
            roundtrip_response(&Response::Pong),
            Response::Pong
        ));
    }

    #[test]
    fn test_response_shutting_down() {
        assert!(matches!(
            roundtrip_response(&Response::ShuttingDown),
            Response::ShuttingDown
        ));
    }

    #[test]
    fn test_response_flushed() {
        assert!(matches!(
            roundtrip_response(&Response::Flushed),
            Response::Flushed
        ));
    }

    #[test]
    fn test_response_error() {
        let resp = Response::Error {
            message: "test error".to_string(),
        };
        match roundtrip_response(&resp) {
            Response::Error { message } => assert_eq!(message, "test error"),
            _ => panic!("unexpected response type"),
        }
    }

    #[test]
    fn test_invalid_json() {
        let result: Result<Request, _> = serde_json::from_slice(b"not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_blob_request_store() {
        let req = BlobRequest::Store {
            media_type: "image/png".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("store"));
        assert!(json.contains("image/png"));
        let parsed: BlobRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, BlobRequest::Store { .. }));
    }

    #[test]
    fn test_blob_request_get_port() {
        let req = BlobRequest::GetPort;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("get_port"));
    }

    #[test]
    fn test_blob_response_stored() {
        let resp = BlobResponse::Stored {
            hash: "abc123".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("abc123"));
    }

    // Notebook protocol tests

    #[test]
    fn test_notebook_request_launch_kernel() {
        let req = NotebookRequest::LaunchKernel {
            kernel_type: "python".into(),
            env_source: "uv:prewarmed".into(),
            notebook_path: Some("/tmp/test.ipynb".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("launch_kernel"));
        assert!(json.contains("python"));

        let parsed: NotebookRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, NotebookRequest::LaunchKernel { .. }));
    }

    #[test]
    fn test_notebook_request_queue_cell() {
        let req = NotebookRequest::QueueCell {
            cell_id: "abc-123".into(),
            code: "print('hello')".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("queue_cell"));
        assert!(json.contains("abc-123"));

        let parsed: NotebookRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            NotebookRequest::QueueCell { cell_id, code } => {
                assert_eq!(cell_id, "abc-123");
                assert_eq!(code, "print('hello')");
            }
            _ => panic!("unexpected request type"),
        }
    }

    #[test]
    fn test_notebook_response_kernel_launched() {
        let resp = NotebookResponse::KernelLaunched {
            kernel_type: "python".into(),
            env_source: "conda:inline".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("kernel_launched"));

        let parsed: NotebookResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, NotebookResponse::KernelLaunched { .. }));
    }

    #[test]
    fn test_notebook_broadcast_output() {
        let broadcast = NotebookBroadcast::Output {
            cell_id: "cell-1".into(),
            output_type: "stream".into(),
            output_json: r#"{"name":"stdout","text":"hello\n"}"#.into(),
        };
        let json = serde_json::to_string(&broadcast).unwrap();
        assert!(json.contains("output"));
        assert!(json.contains("cell-1"));

        let parsed: NotebookBroadcast = serde_json::from_str(&json).unwrap();
        match parsed {
            NotebookBroadcast::Output {
                cell_id,
                output_type,
                ..
            } => {
                assert_eq!(cell_id, "cell-1");
                assert_eq!(output_type, "stream");
            }
            _ => panic!("unexpected broadcast type"),
        }
    }

    #[test]
    fn test_notebook_broadcast_kernel_status() {
        let broadcast = NotebookBroadcast::KernelStatus {
            status: "busy".into(),
            cell_id: Some("cell-1".into()),
        };
        let json = serde_json::to_string(&broadcast).unwrap();
        assert!(json.contains("kernel_status"));
        assert!(json.contains("busy"));
    }

    #[test]
    fn test_notebook_broadcast_comm_sync() {
        let comm = CommSnapshot {
            comm_id: "widget-1".into(),
            target_name: "jupyter.widget".into(),
            state: serde_json::json!({"value": 50}),
            model_module: Some("@jupyter-widgets/controls".into()),
            model_name: Some("IntSliderModel".into()),
            buffers: vec![],
        };
        let broadcast = NotebookBroadcast::CommSync { comms: vec![comm] };
        let json = serde_json::to_string(&broadcast).unwrap();
        assert!(json.contains("comm_sync"));
        assert!(json.contains("widget-1"));
        assert!(json.contains("jupyter.widget"));

        let parsed: NotebookBroadcast = serde_json::from_str(&json).unwrap();
        match parsed {
            NotebookBroadcast::CommSync { comms } => {
                assert_eq!(comms.len(), 1);
                assert_eq!(comms[0].comm_id, "widget-1");
            }
            _ => panic!("unexpected broadcast type"),
        }
    }
}
