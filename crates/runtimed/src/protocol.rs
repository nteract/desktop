//! IPC protocol types for daemon communication.
//!
//! Notebook protocol types (NotebookRequest, NotebookResponse,
//! NotebookBroadcast, etc.) are defined in the `notebook-protocol` crate
//! and re-exported here for backward compatibility.
//!
//! Daemon-internal types (Request, Response, BlobRequest,
//! BlobResponse, DaemonBroadcast) are defined here.

use serde::{Deserialize, Serialize};

use crate::{EnvType, PoolError, PoolStats, PooledEnv};

// Re-export all notebook protocol types from the shared crate.
pub use notebook_protocol::protocol::{
    CommSnapshot, CompletionItem, DenoLaunchedConfig, EnvSyncDiff, HistoryEntry, LaunchedEnvConfig,
    NotebookBroadcast, NotebookRequest, NotebookResponse,
};

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

    /// Shutdown a notebook's kernel and evict its room.
    ShutdownNotebook {
        /// The notebook ID (file path used as identifier).
        notebook_id: String,
    },
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

    /// Notebook shutdown result.
    NotebookShutdown {
        /// Whether the notebook was found and shut down.
        found: bool,
    },
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
// Daemon-internal broadcast
// =============================================================================

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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
            uv_target: 4,
            conda_available: 2,
            conda_warming: 0,
            conda_target: 2,
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
    #[allow(deprecated)]
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
    fn test_notebook_request_execute_cell() {
        let req = NotebookRequest::ExecuteCell {
            cell_id: "cell-456".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("execute_cell"));
        assert!(json.contains("cell-456"));

        let parsed: NotebookRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            NotebookRequest::ExecuteCell { cell_id } => {
                assert_eq!(cell_id, "cell-456");
            }
            _ => panic!("unexpected request type"),
        }
    }

    #[test]
    fn test_notebook_response_kernel_launched() {
        let resp = NotebookResponse::KernelLaunched {
            kernel_type: "python".into(),
            env_source: "conda:inline".into(),
            launched_config: LaunchedEnvConfig {
                conda_deps: Some(vec!["numpy".into(), "pandas".into()]),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("kernel_launched"));
        assert!(json.contains("launched_config"));

        let parsed: NotebookResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, NotebookResponse::KernelLaunched { .. }));
    }

    #[test]
    fn test_notebook_broadcast_output() {
        let broadcast = NotebookBroadcast::Output {
            cell_id: "cell-1".into(),
            output_type: "stream".into(),
            output_json: r#"{"name":"stdout","text":"hello\n"}"#.into(),
            output_index: None,
        };
        let json = serde_json::to_string(&broadcast).unwrap();
        assert!(json.contains("output"));
        assert!(json.contains("cell-1"));
        // output_index is None, so it should be skipped in serialization
        assert!(!json.contains("output_index"));

        let parsed: NotebookBroadcast = serde_json::from_str(&json).unwrap();
        match parsed {
            NotebookBroadcast::Output {
                cell_id,
                output_type,
                output_index,
                ..
            } => {
                assert_eq!(cell_id, "cell-1");
                assert_eq!(output_type, "stream");
                assert_eq!(output_index, None);
            }
            _ => panic!("unexpected broadcast type"),
        }

        // Test with output_index set
        let broadcast_with_index = NotebookBroadcast::Output {
            cell_id: "cell-2".into(),
            output_type: "stream".into(),
            output_json: r#"{"name":"stdout","text":"hello\n"}"#.into(),
            output_index: Some(0),
        };
        let json_with_index = serde_json::to_string(&broadcast_with_index).unwrap();
        assert!(json_with_index.contains("output_index"));

        let parsed_with_index: NotebookBroadcast = serde_json::from_str(&json_with_index).unwrap();
        match parsed_with_index {
            NotebookBroadcast::Output { output_index, .. } => {
                assert_eq!(output_index, Some(0));
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
