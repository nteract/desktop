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

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};
use notebook_sync::{BroadcastReceiver, DocHandle};
use runtimed_client::output_resolver as shared_resolver;
use runtimed_client::resolved_output::DataValue as SharedDataValue;

use crate::error::to_napi_err;

/// Valid cell types accepted by `runCell`.
const VALID_CELL_TYPES: &[&str] = &["code", "markdown", "raw"];

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
    /// Kept alive so the daemon doesn't close the broadcast channel.
    /// Dropped on `close()` to allow clean teardown.
    _broadcast_rx: Option<BroadcastReceiver>,
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

    /// Save the notebook to disk. If a path is given, saves to that path
    /// (creating the file if needed). Otherwise saves to the original location.
    #[napi]
    pub async fn save_notebook(&self, path: Option<String>) -> Result<()> {
        let handle = {
            let st = self.state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone()
        };
        handle.confirm_sync().await.map_err(to_napi_err)?;
        let response = handle
            .send_request(NotebookRequest::SaveNotebook {
                format_cells: false,
                path,
            })
            .await
            .map_err(to_napi_err)?;
        match response {
            NotebookResponse::NotebookSaved { .. } => Ok(()),
            NotebookResponse::Error { error } => Err(Error::from_reason(error)),
            other => Err(Error::from_reason(format!(
                "Unexpected response: {other:?}"
            ))),
        }
    }

    /// Close the session and release the underlying connection.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let mut st = self.state.lock().await;
        st.handle = None;
        st._broadcast_rx = None;
        st.kernel_started = false;
        Ok(())
    }

    /// Add a UV dependency to the notebook (e.g. `"matplotlib>=3.8"`).
    /// Call `syncEnvironment()` or restart the kernel to install it.
    #[napi]
    pub async fn add_uv_dependency(&self, pkg: String) -> Result<()> {
        let handle = {
            let st = self.state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone()
        };
        handle.add_uv_dependency(&pkg).map_err(to_napi_err)?;
        handle.confirm_sync().await.map_err(to_napi_err)?;
        Ok(())
    }

    /// Ask the daemon to hot-install any pending dependency changes into
    /// the running kernel's env. Starts the kernel first if needed.
    /// Returns once the install finishes.
    #[napi]
    pub async fn sync_environment(&self) -> Result<()> {
        ensure_kernel_started(&self.state).await?;
        let handle = {
            let st = self.state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone()
        };
        handle.confirm_sync().await.map_err(to_napi_err)?;
        let response = handle
            .send_request(NotebookRequest::SyncEnvironment { guard: None })
            .await
            .map_err(to_napi_err)?;
        match response {
            NotebookResponse::SyncEnvironmentComplete { .. } => Ok(()),
            NotebookResponse::SyncEnvironmentFailed { error, .. } => Err(Error::from_reason(error)),
            NotebookResponse::GuardRejected { reason } => Err(Error::from_reason(reason)),
            NotebookResponse::Error { error } => Err(Error::from_reason(error)),
            other => Err(Error::from_reason(format!(
                "Unexpected response: {other:?}"
            ))),
        }
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
        if !VALID_CELL_TYPES.contains(&cell_type.as_str()) {
            return Err(Error::from_reason(format!(
                "Invalid cell_type {cell_type:?}. Must be one of: {}",
                VALID_CELL_TYPES.join(", ")
            )));
        }
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
        None,
        vec![],
    )
    .await
    .map_err(to_napi_err)?;

    result
        .handle
        .await_session_ready()
        .await
        .map_err(to_napi_err)?;

    let notebook_id = result.info.notebook_id.clone();
    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    let state = SessionState {
        handle: Some(result.handle),
        _broadcast_rx: Some(result.broadcast_rx),
        kernel_started: false,
        runtime,
        blob_base_url,
        blob_store_path,
        socket_path,
        working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
    };

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

    result
        .handle
        .await_session_ready()
        .await
        .map_err(to_napi_err)?;

    let (blob_base_url, blob_store_path) = resolve_blob_paths(&socket_path).await;

    // Try to hydrate kernel state from the RuntimeStateDoc.
    let (kernel_started, runtime) = {
        let rs = result.handle.get_runtime_state().ok();
        let started = rs
            .as_ref()
            .map(|r| {
                matches!(
                    r.kernel.lifecycle,
                    runtime_doc::RuntimeLifecycle::Running(_)
                )
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
        _broadcast_rx: Some(result.broadcast_rx),
        kernel_started,
        runtime,
        blob_base_url,
        blob_store_path,
        socket_path,
        working_dir: None,
    };

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
    handle.confirm_sync().await.map_err(to_napi_err)?;
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
        NotebookResponse::GuardRejected { reason } => Err(Error::from_reason(reason)),
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

    let helper_timeout = Duration::from_secs(60 * 60); // 1 hour ceiling; caller's tokio::time::timeout is the real bound
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
            let outputs: Vec<JsOutput> = resolved
                .into_iter()
                .map(to_js_output)
                .collect::<napi::Result<Vec<_>>>()?;
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

fn to_js_output(o: runtimed_client::resolved_output::Output) -> napi::Result<JsOutput> {
    js_output_from_resolved_output(o)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize output data: {e}")))
}

fn js_output_from_resolved_output(
    o: runtimed_client::resolved_output::Output,
) -> serde_json::Result<JsOutput> {
    let data_json = match o.data {
        Some(d) => {
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
            Some(serde_json::to_string(&map)?)
        }
        None => None,
    };
    let blob_urls_json = o.blob_urls.map(|m| serde_json::to_string(&m)).transpose()?;
    let blob_paths_json = o
        .blob_paths
        .map(|m| serde_json::to_string(&m))
        .transpose()?;
    Ok(JsOutput {
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
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use runtimed_client::resolved_output::{DataValue as SharedDataValue, Output as SharedOutput};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn resolve_socket_path_prefers_explicit_override() {
        let path = resolve_socket_path(Some("/tmp/runtimed-node-test.sock".to_string()));
        assert_eq!(path, PathBuf::from("/tmp/runtimed-node-test.sock"));
    }

    #[test]
    fn to_js_output_preserves_output_fields_and_typed_data_contract() {
        let data = HashMap::from([
            (
                "text/plain".to_string(),
                SharedDataValue::Text("hello".to_string()),
            ),
            (
                "image/png".to_string(),
                SharedDataValue::Binary(vec![0, 1, 2, 255]),
            ),
            (
                "application/json".to_string(),
                SharedDataValue::Json(json!({"ok": true, "n": 3})),
            ),
        ]);
        let mut output = SharedOutput::execute_result(data, 7);
        output.blob_urls = Some(HashMap::from([(
            "image/png".to_string(),
            "http://localhost:9999/blob/abc".to_string(),
        )]));
        output.blob_paths = Some(HashMap::from([(
            "image/png".to_string(),
            "/tmp/blobs/ab/abc".to_string(),
        )]));

        let js_output = js_output_from_resolved_output(output).expect("output serializes");

        assert_eq!(js_output.output_type, "execute_result");
        assert_eq!(js_output.execution_count, Some(7));

        let data_json: serde_json::Value =
            serde_json::from_str(js_output.data_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            data_json["text/plain"],
            json!({"type": "text", "value": "hello"})
        );
        assert_eq!(
            data_json["image/png"],
            json!({"type": "binary", "value": "AAEC/w=="})
        );
        assert_eq!(
            data_json["application/json"],
            json!({"type": "json", "value": {"ok": true, "n": 3}})
        );

        let blob_urls: serde_json::Value =
            serde_json::from_str(js_output.blob_urls_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            blob_urls,
            json!({"image/png": "http://localhost:9999/blob/abc"})
        );

        let blob_paths: serde_json::Value =
            serde_json::from_str(js_output.blob_paths_json.as_deref().unwrap()).unwrap();
        assert_eq!(blob_paths, json!({"image/png": "/tmp/blobs/ab/abc"}));
    }

    #[test]
    fn to_js_output_keeps_kernel_error_shape_stable() {
        let output = SharedOutput::error(
            "ValueError",
            "bad value",
            vec![
                "Traceback line 1".to_string(),
                "Traceback line 2".to_string(),
            ],
        );

        let js_output = js_output_from_resolved_output(output).expect("error output serializes");

        assert_eq!(js_output.output_type, "error");
        assert_eq!(js_output.ename.as_deref(), Some("ValueError"));
        assert_eq!(js_output.evalue.as_deref(), Some("bad value"));
        assert_eq!(
            js_output.traceback,
            Some(vec![
                "Traceback line 1".to_string(),
                "Traceback line 2".to_string()
            ])
        );
        assert!(js_output.data_json.is_none());
        assert!(js_output.blob_urls_json.is_none());
        assert!(js_output.blob_paths_json.is_none());
    }
}
