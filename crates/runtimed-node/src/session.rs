//! `Session` — the user-facing handle for a notebook + kernel.
//!
//! Phase 1: minimal surface duplicated from `runtimed-py/src/session_core.rs`.
//! Phase 2 will extract the shared logic into a `runtimed-session` crate
//! and collapse both `-py` and `-node` onto it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Serialize;
use tokio::sync::Mutex;

use notebook_doc::runtime_state::CommDocEntry;
use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};
use notebook_sync::DocHandle;
use runtimed_client::output_resolver as shared_resolver;
use runtimed_client::resolved_output::DataValue as SharedDataValue;

use crate::error::to_napi_err;

// ── Options ────────────────────────────────────────────────────────────

/// Options for `createNotebook()`.
#[napi(object)]
#[derive(Default)]
pub struct CreateNotebookOptions {
    /// Runtime type: `"python"` or `"deno"`. Defaults to `"python"`.
    pub runtime: Option<String>,
    /// Working directory for the kernel.
    pub working_dir: Option<String>,
    /// Override daemon socket path (otherwise uses `default_socket_path()`).
    pub socket_path: Option<String>,
    /// Actor label for presence / Automerge provenance.
    pub peer_label: Option<String>,
}

/// Options for `openNotebook()`.
#[napi(object)]
#[derive(Default)]
pub struct OpenNotebookOptions {
    pub socket_path: Option<String>,
    pub peer_label: Option<String>,
}

/// Options for `Session.runCell()`.
#[napi(object)]
#[derive(Default)]
pub struct RunCellOptions {
    /// Max milliseconds to wait for execution. Default 120_000 (2 min).
    pub timeout_ms: Option<u32>,
    /// Cell source type: `"code"` (default) or `"markdown"`.
    pub cell_type: Option<String>,
}

// ── Outputs (serialized to JS via serde_json) ──────────────────────────

/// One output from a cell. `data` values are: `{ type: "text"|"binary"|"json", value: ... }`.
#[napi(object)]
pub struct JsOutput {
    pub output_type: String,
    pub name: Option<String>,
    pub text: Option<String>,
    /// MIME-keyed map, serialized via JSON. Binary values become base64 strings.
    pub data_json: Option<String>,
    pub ename: Option<String>,
    pub evalue: Option<String>,
    pub traceback: Option<Vec<String>>,
    pub execution_count: Option<i64>,
    pub blob_urls_json: Option<String>,
    pub blob_paths_json: Option<String>,
}

/// Result of running a cell.
#[napi(object)]
pub struct CellResult {
    pub cell_id: String,
    pub execution_count: Option<i64>,
    /// One of: `"done"`, `"error"`, `"timeout"`, `"kernel_error"`.
    pub status: String,
    pub success: bool,
    pub outputs: Vec<JsOutput>,
}

// ── Internal state ─────────────────────────────────────────────────────

struct SessionState {
    handle: Option<DocHandle>,
    kernel_started: bool,
    runtime: String,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
    #[allow(dead_code)]
    socket_path: PathBuf,
    working_dir: Option<String>,
}

// ── Session ────────────────────────────────────────────────────────────

/// A connected notebook session.
#[napi]
pub struct Session {
    notebook_id: String,
    state: Arc<Mutex<SessionState>>,
}

#[napi]
impl Session {
    /// The notebook ID (always a UUID).
    #[napi(getter)]
    pub fn notebook_id(&self) -> String {
        self.notebook_id.clone()
    }

    /// Close the session and release the underlying connection.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let mut st = self.state.lock().await;
        st.handle = None;
        st.kernel_started = false;
        Ok(())
    }

    /// Execute a source string as a new cell. Starts the kernel on demand.
    /// Returns the cell's outputs once execution is terminal.
    #[napi]
    pub async fn run_cell(
        &self,
        source: String,
        options: Option<RunCellOptions>,
    ) -> Result<CellResult> {
        let opts = options.unwrap_or_default();
        let cell_type = opts.cell_type.unwrap_or_else(|| "code".to_string());
        let timeout = Duration::from_millis(opts.timeout_ms.unwrap_or(120_000) as u64);

        // 1. Ensure kernel started (lazy).
        ensure_kernel_started(&self.state).await?;

        // 2. Add a new cell with source (single atomic mutation).
        let cell_id = format!("cell-{}", uuid::Uuid::new_v4());
        {
            let st = self.state.lock().await;
            let handle = st
                .handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?;
            handle
                .add_cell_with_source(&cell_id, &cell_type, None, &source)
                .map_err(to_napi_err)?;
        }

        // 3. Queue the cell and wait for completion.
        let result = tokio::time::timeout(timeout, async {
            let execution_id = queue_cell(&self.state, &cell_id).await?;
            collect_outputs(&self.state, &cell_id, &execution_id).await
        })
        .await;

        match result {
            Ok(Ok(cell_result)) => Ok(cell_result),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(CellResult {
                cell_id,
                execution_count: None,
                status: "timeout".to_string(),
                success: false,
                outputs: vec![],
            }),
        }
    }
}

// ── Standalone constructors ────────────────────────────────────────────

/// Create a new (ephemeral) notebook against the daemon.
#[napi]
pub async fn create_notebook(options: Option<CreateNotebookOptions>) -> Result<Session> {
    let opts = options.unwrap_or_default();
    let runtime = opts.runtime.unwrap_or_else(|| "python".to_string());
    let socket_path = resolve_socket_path(opts.socket_path);
    let working_dir: Option<PathBuf> = opts.working_dir.map(PathBuf::from);
    let actor_label = opts
        .peer_label
        .clone()
        .unwrap_or_else(|| "runtimed-node".to_string());

    let result = notebook_sync::connect::connect_create(
        socket_path.clone(),
        &runtime,
        working_dir.clone(),
        &actor_label,
        /* ephemeral */ false,
    )
    .await
    .map_err(to_napi_err)?;

    let notebook_id = result.info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    let state = SessionState {
        handle: Some(result.handle),
        kernel_started: false,
        runtime,
        blob_base_url,
        blob_store_path,
        socket_path,
        working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
    };
    // Keep broadcast_rx alive for the session. We don't use it directly
    // in Phase 1 — collect_outputs reads the Automerge doc — but dropping
    // it would close the broadcast channel on the daemon side. Leak-into-Arc
    // is the simplest way.
    std::mem::forget(result.broadcast_rx);

    Ok(Session {
        notebook_id,
        state: Arc::new(Mutex::new(state)),
    })
}

/// Open an existing notebook by ID.
#[napi]
pub async fn open_notebook(
    notebook_id: String,
    options: Option<OpenNotebookOptions>,
) -> Result<Session> {
    let opts = options.unwrap_or_default();
    let socket_path = resolve_socket_path(opts.socket_path);
    let actor_label = opts
        .peer_label
        .clone()
        .unwrap_or_else(|| "runtimed-node".to_string());

    let result =
        notebook_sync::connect::connect(socket_path.clone(), notebook_id.clone(), &actor_label)
            .await
            .map_err(to_napi_err)?;

    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    // Try to hydrate kernel state from the RuntimeStateDoc.
    let (kernel_started, runtime) = {
        let rs = result.handle.get_runtime_state().ok();
        let started = rs
            .as_ref()
            .map(|r| {
                r.kernel.status == "ready" || r.kernel.status == "busy" || r.kernel.status == "idle"
            })
            .unwrap_or(false);
        let runtime = rs
            .as_ref()
            .map(|r| r.kernel.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "python".to_string());
        (started, runtime)
    };

    let state = SessionState {
        handle: Some(result.handle),
        kernel_started,
        runtime,
        blob_base_url,
        blob_store_path,
        socket_path,
        working_dir: None,
    };
    std::mem::forget(result.broadcast_rx);

    Ok(Session {
        notebook_id,
        state: Arc::new(Mutex::new(state)),
    })
}

// ── Internals ──────────────────────────────────────────────────────────

fn resolve_socket_path(override_path: Option<String>) -> PathBuf {
    override_path
        .map(PathBuf::from)
        .unwrap_or_else(runt_workspace::default_socket_path)
}

async fn resolve_blob_paths(socket_path: &std::path::Path) -> (Option<String>, Option<PathBuf>) {
    if let Some(parent) = socket_path.parent() {
        let daemon_json = parent.join("daemon.json");
        let base_url = if daemon_json.exists() {
            tokio::fs::read_to_string(&daemon_json)
                .await
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("blob_port").and_then(|p| p.as_u64()))
                .map(|port| format!("http://localhost:{port}"))
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

async fn ensure_kernel_started(state: &Arc<Mutex<SessionState>>) -> Result<()> {
    let (started, runtime, notebook_path) = {
        let st = state.lock().await;
        (
            st.kernel_started,
            st.runtime.clone(),
            st.working_dir.clone(),
        )
    };
    if started {
        return Ok(());
    }
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| Error::from_reason("Not connected"))?
            .clone()
    };
    let response = handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type: runtime.clone(),
            env_source: "auto".to_string(),
            notebook_path,
        })
        .await
        .map_err(to_napi_err)?;

    match response {
        NotebookResponse::KernelLaunched { .. } | NotebookResponse::KernelAlreadyRunning { .. } => {
            let mut st = state.lock().await;
            st.kernel_started = true;
            Ok(())
        }
        NotebookResponse::Error { error } => Err(Error::from_reason(error)),
        other => Err(Error::from_reason(format!(
            "Unexpected response: {other:?}"
        ))),
    }
}

async fn queue_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> Result<String> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| Error::from_reason("Not connected"))?
            .clone()
    };
    handle.confirm_sync().await.map_err(to_napi_err)?;
    let response = handle
        .send_request(NotebookRequest::ExecuteCell {
            cell_id: cell_id.to_string(),
        })
        .await
        .map_err(to_napi_err)?;
    match response {
        NotebookResponse::CellQueued { execution_id, .. } => Ok(execution_id),
        NotebookResponse::Error { error } => Err(Error::from_reason(error)),
        other => Err(Error::from_reason(format!(
            "Unexpected response: {other:?}"
        ))),
    }
}

async fn collect_outputs(
    state: &Arc<Mutex<SessionState>>,
    cell_id: &str,
    execution_id: &str,
) -> Result<CellResult> {
    let (handle, blob_base_url, blob_store_path) = {
        let st = state.lock().await;
        (
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone(),
            st.blob_base_url.clone(),
            st.blob_store_path.clone(),
        )
    };

    let helper_timeout = Duration::from_secs(60 * 60 * 24);
    let terminal =
        notebook_sync::await_execution_terminal(&handle, execution_id, helper_timeout, None).await;

    match terminal {
        Ok(terminal_state) => {
            let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
            let resolved = shared_resolver::resolve_cell_outputs(
                &terminal_state.output_manifests,
                &blob_base_url,
                &blob_store_path,
                comms.as_ref(),
            )
            .await;
            let outputs: Vec<JsOutput> = resolved.into_iter().map(to_js_output).collect();
            let success = terminal_state.status == "done"
                && terminal_state.success
                && !outputs.iter().any(|o| o.output_type == "error");
            Ok(CellResult {
                cell_id: cell_id.to_string(),
                execution_count: terminal_state.execution_count,
                status: terminal_state.status,
                success,
                outputs,
            })
        }
        Err(notebook_sync::ExecutionTerminalError::KernelFailed { reason }) => Ok(CellResult {
            cell_id: cell_id.to_string(),
            execution_count: None,
            status: "kernel_error".to_string(),
            success: false,
            outputs: vec![JsOutput {
                output_type: "error".to_string(),
                name: None,
                text: None,
                data_json: None,
                ename: Some("KernelError".to_string()),
                evalue: Some(reason),
                traceback: None,
                execution_count: None,
                blob_urls_json: None,
                blob_paths_json: None,
            }],
        }),
        Err(notebook_sync::ExecutionTerminalError::Timeout) => {
            Err(Error::from_reason("Execution wait aborted"))
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
enum DataValueJson {
    Text(String),
    /// base64-encoded.
    Binary(String),
    Json(serde_json::Value),
}

fn to_js_output(o: runtimed_client::resolved_output::Output) -> JsOutput {
    let data_json = o.data.map(|d| {
        let map: std::collections::HashMap<String, DataValueJson> = d
            .into_iter()
            .map(|(k, v)| {
                let dv = match v {
                    SharedDataValue::Text(s) => DataValueJson::Text(s),
                    SharedDataValue::Binary(b) => {
                        use base64::{engine::general_purpose::STANDARD, Engine as _};
                        DataValueJson::Binary(STANDARD.encode(&b))
                    }
                    SharedDataValue::Json(j) => DataValueJson::Json(j),
                };
                (k, dv)
            })
            .collect();
        serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
    });
    let blob_urls_json = o
        .blob_urls
        .map(|m| serde_json::to_string(&m).unwrap_or_default());
    let blob_paths_json = o
        .blob_paths
        .map(|m| serde_json::to_string(&m).unwrap_or_default());
    JsOutput {
        output_type: o.output_type,
        name: o.name,
        text: o.text,
        data_json,
        ename: o.ename,
        evalue: o.evalue,
        traceback: o.traceback,
        execution_count: o.execution_count,
        blob_urls_json,
        blob_paths_json,
    }
}

// Suppress unused warning; the helper is invoked by to_js_output via impl.
#[allow(dead_code)]
fn _comm_keeper(_c: &CommDocEntry) {}
