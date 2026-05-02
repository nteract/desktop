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

type CommMap = std::collections::HashMap<String, runtime_doc::CommDocEntry>;

// ── Options ────────────────────────────────────────────────────────────

/// Package manager for Python notebook dependencies.
///
/// This mirrors `notebook_protocol::connection::PackageManager` at the N-API
/// boundary. We cannot attach `#[napi]` to the protocol crate's external enum,
/// and that enum intentionally includes an `Unknown(String)` wire-compatibility
/// variant that should not be exposed as a typed JavaScript option.
#[napi(string_enum = "lowercase")]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
}

impl From<PackageManager> for notebook_protocol::connection::PackageManager {
    fn from(value: PackageManager) -> Self {
        match value {
            PackageManager::Uv => Self::Uv,
            PackageManager::Conda => Self::Conda,
            PackageManager::Pixi => Self::Pixi,
        }
    }
}

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
    /// Human-readable session description. Used as the peer label when `peerLabel` is omitted.
    pub description: Option<String>,
    /// Packages to record before the kernel starts (for example `["numpy", "matplotlib"]`).
    /// Python notebooks use the selected package manager; Deno notebooks treat these as
    /// runtime-native import dependencies.
    pub dependencies: Option<Vec<String>>,
    /// Package manager for Python dependencies. Defaults to the daemon/user setting.
    pub package_manager: Option<PackageManager>,
}

/// Options for `openNotebook()`.
#[napi(object)]
#[derive(Default)]
pub struct OpenNotebookOptions {
    pub socket_path: Option<String>,
    pub peer_label: Option<String>,
}

/// Options for dependency edit methods.
#[napi(object)]
#[derive(Default)]
pub struct DependencyEditOptions {
    /// Dependency manager to edit. Defaults to `uv` for Python notebooks.
    pub package_manager: Option<PackageManager>,
}

/// Options for `Session.runCell()`.
#[napi(object)]
#[derive(Default)]
pub struct RunCellOptions {
    /// Max milliseconds to wait for execution. Default 120_000 (2 min).
    pub timeout_ms: Option<u32>,
    /// Cell source type: `"code"` (default), `"markdown"`, or `"raw"`.
    pub cell_type: Option<String>,
}

/// Options for `Session.createCell()`.
#[napi(object)]
#[derive(Default)]
pub struct CreateCellOptions {
    /// Cell source type: `"code"` (default), `"markdown"`, or `"raw"`.
    pub cell_type: Option<String>,
    /// Insert after this cell, or omit to insert at the beginning.
    pub after_cell_id: Option<String>,
}

/// Options for `Session.setCell()`.
#[napi(object)]
#[derive(Default)]
pub struct SetCellOptions {
    /// New source text. Omit to leave unchanged.
    pub source: Option<String>,
    /// New cell source type. Omit to leave unchanged.
    pub cell_type: Option<String>,
}

/// Options for `Session.moveCell()`.
#[napi(object)]
#[derive(Default)]
pub struct MoveCellOptions {
    /// Move after this cell, or omit/null to move to the beginning.
    pub after_cell_id: Option<String>,
}

/// Options for `Session.executeCell()`.
#[napi(object)]
#[derive(Default)]
pub struct ExecuteCellOptions {
    /// Max milliseconds to wait for execution. Default 120_000 (2 min).
    pub timeout_ms: Option<u32>,
}

/// Options for `Session.queueCell()`.
#[napi(object)]
#[derive(Default)]
pub struct QueueCellOptions {
    /// Cell source type: `"code"` (default), `"markdown"`, or `"raw"`.
    pub cell_type: Option<String>,
}

/// Options for `Session.waitForExecution()`.
#[napi(object)]
#[derive(Default)]
pub struct WaitExecutionOptions {
    /// Cell ID to use when reporting kernel failures before terminal state syncs.
    pub cell_id: Option<String>,
    /// Max milliseconds to wait for execution. Default 120_000 (2 min).
    pub timeout_ms: Option<u32>,
}

/// Options for top-level `getExecutionResult()`.
#[napi(object)]
#[derive(Default)]
pub struct GetExecutionResultOptions {
    /// Override daemon socket path (otherwise uses `defaultSocketPath()`).
    pub socket_path: Option<String>,
}

/// A queued execution handle.
#[napi(object)]
pub struct QueuedExecution {
    pub cell_id: String,
    pub execution_id: String,
}

/// A notebook cell snapshot.
#[napi(object)]
pub struct JsCellSnapshot {
    pub id: String,
    pub cell_type: String,
    pub position: String,
    pub source: String,
    /// JSON-encoded metadata object.
    pub metadata_json: String,
    /// Legacy execution count as stored in the notebook doc.
    pub execution_count: Option<String>,
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
    pub execution_id: String,
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

struct OutputResolutionContext<'a> {
    comms: Option<&'a CommMap>,
    blob_base_url: &'a Option<String>,
    blob_store_path: &'a Option<PathBuf>,
}

struct ExecutionResultParts<'a> {
    execution_id: &'a str,
    cell_id: &'a str,
    status: &'a str,
    success: bool,
    execution_count: Option<i64>,
    output_manifests: &'a [serde_json::Value],
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

    /// Add a dependency to the selected package manager (defaults to UV).
    /// Call `syncEnvironment()` or restart the kernel to install it.
    #[napi]
    pub async fn add_dependency(
        &self,
        pkg: String,
        options: Option<DependencyEditOptions>,
    ) -> Result<()> {
        let handle = session_handle(&self.state).await?;
        let package_manager = dependency_package_manager(options);
        match package_manager {
            PackageManager::Uv => handle.add_uv_dependency(&pkg),
            PackageManager::Conda => handle.add_conda_dependency(&pkg),
            PackageManager::Pixi => handle.add_pixi_dependency(&pkg),
        }
        .map_err(to_napi_err)?;
        approve_current_trust(&handle, None).await
    }

    /// Remove a dependency from the selected package manager (defaults to UV).
    /// Returns true if a dependency was removed.
    #[napi]
    pub async fn remove_dependency(
        &self,
        pkg: String,
        options: Option<DependencyEditOptions>,
    ) -> Result<bool> {
        let handle = session_handle(&self.state).await?;
        let package_manager = dependency_package_manager(options);
        let removed = match package_manager {
            PackageManager::Uv => handle.remove_uv_dependency(&pkg),
            PackageManager::Conda => handle.remove_conda_dependency(&pkg),
            PackageManager::Pixi => handle.remove_pixi_dependency(&pkg),
        }
        .map_err(to_napi_err)?;
        if removed {
            approve_current_trust(&handle, None).await?;
        }
        Ok(removed)
    }

    /// Add a UV dependency to the notebook (e.g. `"matplotlib>=3.8"`).
    /// Call `syncEnvironment()` or restart the kernel to install it.
    #[napi]
    pub async fn add_uv_dependency(&self, pkg: String) -> Result<()> {
        self.add_dependency(
            pkg,
            Some(DependencyEditOptions {
                package_manager: Some(PackageManager::Uv),
            }),
        )
        .await
    }

    /// Get the current dependency fingerprint for diagnostics.
    #[napi]
    pub async fn dependency_fingerprint(&self) -> Result<Option<String>> {
        let handle = {
            let st = self.state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone()
        };
        Ok(dependency_fingerprint_for_handle(&handle))
    }

    /// Approve and sign the current dependency metadata.
    #[napi]
    pub async fn approve_trust(&self, observed_heads: Option<Vec<String>>) -> Result<()> {
        let handle = {
            let st = self.state.lock().await;
            st.handle
                .as_ref()
                .ok_or_else(|| Error::from_reason("Not connected"))?
                .clone()
        };
        approve_current_trust(&handle, observed_heads).await
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

    /// Return all cells in notebook order.
    #[napi]
    pub async fn list_cells(&self) -> Result<Vec<JsCellSnapshot>> {
        let handle = session_handle(&self.state).await?;
        Ok(handle
            .get_cells()
            .into_iter()
            .map(js_cell_from_snapshot)
            .collect())
    }

    /// Return one cell by ID, or null if it does not exist.
    #[napi]
    pub async fn get_cell(&self, cell_id: String) -> Result<Option<JsCellSnapshot>> {
        let handle = session_handle(&self.state).await?;
        Ok(handle.get_cell(&cell_id).map(js_cell_from_snapshot))
    }

    /// Create a cell without executing it. Returns the new cell ID.
    #[napi]
    pub async fn create_cell(
        &self,
        source: String,
        options: Option<CreateCellOptions>,
    ) -> Result<String> {
        let opts = options.unwrap_or_default();
        let cell_type = normalize_cell_type(opts.cell_type)?;
        let handle = session_handle(&self.state).await?;
        let cell_id = format!("cell-{}", uuid::Uuid::new_v4());
        handle
            .add_cell_with_source(&cell_id, &cell_type, opts.after_cell_id.as_deref(), &source)
            .map_err(to_napi_err)?;
        Ok(cell_id)
    }

    /// Replace a cell's source and/or type. Returns true if the cell existed.
    #[napi]
    pub async fn set_cell(&self, cell_id: String, options: SetCellOptions) -> Result<bool> {
        let cell_type = options
            .cell_type
            .map(|cell_type| normalize_cell_type(Some(cell_type)))
            .transpose()?;
        let handle = session_handle(&self.state).await?;
        let mut found = false;
        if let Some(source) = options.source {
            found |= handle
                .update_source(&cell_id, &source)
                .map_err(to_napi_err)?;
        }
        if let Some(cell_type) = cell_type {
            found |= handle
                .set_cell_type(&cell_id, &cell_type)
                .map_err(to_napi_err)?;
        }
        Ok(found)
    }

    /// Delete a cell. Returns true if the cell existed.
    #[napi]
    pub async fn delete_cell(&self, cell_id: String) -> Result<bool> {
        let handle = session_handle(&self.state).await?;
        handle.delete_cell(&cell_id).map_err(to_napi_err)
    }

    /// Move a cell after another cell, or to the beginning when omitted/null.
    /// Returns the new position string.
    #[napi]
    pub async fn move_cell(
        &self,
        cell_id: String,
        options: Option<MoveCellOptions>,
    ) -> Result<String> {
        let handle = session_handle(&self.state).await?;
        let after_cell_id = options.and_then(|opts| opts.after_cell_id);
        handle
            .move_cell(&cell_id, after_cell_id.as_deref())
            .map_err(to_napi_err)
    }

    /// Execute an existing code cell and wait for terminal outputs.
    #[napi]
    pub async fn execute_cell(
        &self,
        cell_id: String,
        options: Option<ExecuteCellOptions>,
    ) -> Result<CellResult> {
        let opts = options.unwrap_or_default();
        let timeout = Duration::from_millis(opts.timeout_ms.unwrap_or(120_000) as u64);
        ensure_kernel_started(&self.state).await?;
        let execution_id = queue_existing_cell(&self.state, &cell_id).await?;

        let result = tokio::time::timeout(timeout, async {
            collect_outputs(&self.state, &cell_id, &execution_id).await
        })
        .await;

        match result {
            Ok(Ok(cell_result)) => Ok(cell_result),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(CellResult {
                cell_id,
                execution_id,
                execution_count: None,
                status: "timeout".to_string(),
                success: false,
                outputs: vec![],
            }),
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
        let cell_type = normalize_cell_type(opts.cell_type)?;
        let timeout = Duration::from_millis(opts.timeout_ms.unwrap_or(120_000) as u64);

        // 1. Ensure kernel started (lazy).
        ensure_kernel_started(&self.state).await?;

        // 2. Add a new cell with source (single atomic mutation).
        let cell_id = add_source_cell(&self.state, &source, &cell_type).await?;

        // 3. Queue the cell. Do this before the timeout wrapper so timeout
        // results still carry an execution ID that can be recovered later.
        let execution_id = queue_existing_cell(&self.state, &cell_id).await?;

        let result = tokio::time::timeout(timeout, async {
            collect_outputs(&self.state, &cell_id, &execution_id).await
        })
        .await;

        match result {
            Ok(Ok(cell_result)) => Ok(cell_result),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(CellResult {
                cell_id,
                execution_id,
                execution_count: None,
                status: "timeout".to_string(),
                success: false,
                outputs: vec![],
            }),
        }
    }

    /// Queue a source string as a new cell without waiting for execution.
    #[napi]
    pub async fn queue_cell(
        &self,
        source: String,
        options: Option<QueueCellOptions>,
    ) -> Result<QueuedExecution> {
        let opts = options.unwrap_or_default();
        let cell_type = normalize_cell_type(opts.cell_type)?;

        ensure_kernel_started(&self.state).await?;
        let cell_id = add_source_cell(&self.state, &source, &cell_type).await?;
        let execution_id = queue_existing_cell(&self.state, &cell_id).await?;

        Ok(QueuedExecution {
            cell_id,
            execution_id,
        })
    }

    /// Wait for an already-queued execution in this live session.
    #[napi]
    pub async fn wait_for_execution(
        &self,
        execution_id: String,
        options: Option<WaitExecutionOptions>,
    ) -> Result<CellResult> {
        let opts = options.unwrap_or_default();
        let timeout = Duration::from_millis(opts.timeout_ms.unwrap_or(120_000) as u64);
        let fallback_cell_id = opts.cell_id.unwrap_or_default();

        let result = tokio::time::timeout(timeout, async {
            collect_outputs(&self.state, &fallback_cell_id, &execution_id).await
        })
        .await;

        match result {
            Ok(Ok(cell_result)) => Ok(cell_result),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(CellResult {
                cell_id: fallback_cell_id,
                execution_id,
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
        .or_else(|| opts.description.map(|desc| format!("runtimed-node:{desc}")))
        .unwrap_or_else(|| "runtimed-node".to_string());
    let dependencies = opts.dependencies.unwrap_or_default();
    let package_manager = opts.package_manager.map(Into::into);

    let result = notebook_sync::connect::connect_create(
        socket_path.clone(),
        &runtime,
        working_dir.clone(),
        &actor_label,
        /* ephemeral */ false,
        package_manager,
        dependencies,
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

/// Read a terminal execution result from the daemon's durable execution store.
#[napi]
pub async fn get_execution_result(
    execution_id: String,
    options: Option<GetExecutionResultOptions>,
) -> Result<CellResult> {
    let opts = options.unwrap_or_default();
    let socket_path = resolve_socket_path(opts.socket_path);
    let info = runtimed_client::singleton::query_daemon_info(socket_path.clone()).await;
    let execution_store_dir = execution_store_dir_for_socket(&socket_path, info.as_ref());
    let store = runtimed_client::execution_store::ExecutionStore::new(execution_store_dir);
    let record = store.read_record(&execution_id).await.ok_or_else(|| {
        Error::from_reason(format!(
            "Execution not found in durable store: {execution_id}"
        ))
    })?;

    cell_result_from_record(&socket_path, record).await
}

// ── Internals ──────────────────────────────────────────────────────────

fn resolve_socket_path(override_path: Option<String>) -> PathBuf {
    override_path
        .map(PathBuf::from)
        .unwrap_or_else(runt_workspace::default_socket_path)
}

fn execution_store_dir_for_socket(
    socket_path: &std::path::Path,
    daemon_info: Option<&runtimed_client::singleton::DaemonInfo>,
) -> PathBuf {
    if let Some(dir) = daemon_info.and_then(|info| info.execution_store_dir.as_ref()) {
        return PathBuf::from(dir);
    }

    socket_path
        .parent()
        .map(|parent| parent.join("executions"))
        .unwrap_or_else(runtimed_client::default_execution_store_dir)
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

async fn session_handle(state: &Arc<Mutex<SessionState>>) -> Result<DocHandle> {
    let st = state.lock().await;
    st.handle
        .as_ref()
        .ok_or_else(|| Error::from_reason("Not connected"))
        .cloned()
}

fn dependency_package_manager(options: Option<DependencyEditOptions>) -> PackageManager {
    options
        .and_then(|opts| opts.package_manager)
        .unwrap_or(PackageManager::Uv)
}

fn js_cell_from_snapshot(cell: notebook_doc::CellSnapshot) -> JsCellSnapshot {
    JsCellSnapshot {
        id: cell.id,
        cell_type: cell.cell_type,
        position: cell.position,
        source: cell.source,
        metadata_json: serde_json::to_string(&cell.metadata).unwrap_or_else(|_| "{}".to_string()),
        execution_count: if cell.execution_count == "null" {
            None
        } else {
            Some(cell.execution_count)
        },
    }
}

fn normalize_cell_type(cell_type: Option<String>) -> Result<String> {
    let cell_type = cell_type.unwrap_or_else(|| "code".to_string());
    if !VALID_CELL_TYPES.contains(&cell_type.as_str()) {
        return Err(Error::from_reason(format!(
            "Invalid cell_type {cell_type:?}. Must be one of: {}",
            VALID_CELL_TYPES.join(", ")
        )));
    }
    Ok(cell_type)
}

async fn add_source_cell(
    state: &Arc<Mutex<SessionState>>,
    source: &str,
    cell_type: &str,
) -> Result<String> {
    let cell_id = format!("cell-{}", uuid::Uuid::new_v4());
    {
        let st = state.lock().await;
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| Error::from_reason("Not connected"))?;
        handle
            .add_cell_with_source(&cell_id, cell_type, None, source)
            .map_err(to_napi_err)?;
    }
    Ok(cell_id)
}

fn dependency_fingerprint_for_handle(handle: &DocHandle) -> Option<String> {
    handle
        .get_notebook_metadata()
        .map(|snapshot| snapshot.dependency_fingerprint())
}

async fn approve_current_trust(
    handle: &DocHandle,
    observed_heads: Option<Vec<String>>,
) -> Result<()> {
    handle.confirm_sync().await.map_err(to_napi_err)?;
    let response = handle
        .send_request(NotebookRequest::ApproveTrust { observed_heads })
        .await
        .map_err(to_napi_err)?;

    match response {
        NotebookResponse::Ok {} => Ok(()),
        NotebookResponse::GuardRejected { reason } => Err(Error::from_reason(reason)),
        NotebookResponse::Error { error } => Err(Error::from_reason(error)),
        other => Err(Error::from_reason(format!(
            "Unexpected response: {other:?}"
        ))),
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
            env_source: notebook_protocol::connection::LaunchSpec::Auto,
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

async fn queue_existing_cell(state: &Arc<Mutex<SessionState>>, cell_id: &str) -> Result<String> {
    let handle = {
        let st = state.lock().await;
        st.handle
            .as_ref()
            .ok_or_else(|| Error::from_reason("Not connected"))?
            .clone()
    };
    let required_heads = handle.current_heads_hex().map_err(to_napi_err)?;
    let response = handle
        .send_request_after_heads(
            NotebookRequest::ExecuteCell {
                cell_id: cell_id.to_string(),
            },
            required_heads,
        )
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
            cell_result_from_output_manifests(
                ExecutionResultParts {
                    execution_id,
                    cell_id,
                    status: &terminal_state.status,
                    success: terminal_state.success,
                    execution_count: terminal_state.execution_count,
                    output_manifests: &terminal_state.output_manifests,
                },
                OutputResolutionContext {
                    comms: comms.as_ref(),
                    blob_base_url: &blob_base_url,
                    blob_store_path: &blob_store_path,
                },
            )
            .await
        }
        Err(notebook_sync::ExecutionTerminalError::KernelFailed { reason }) => Ok(CellResult {
            cell_id: cell_id.to_string(),
            execution_id: execution_id.to_string(),
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

async fn cell_result_from_record(
    socket_path: &std::path::Path,
    record: runtimed_client::execution_store::ExecutionRecord,
) -> Result<CellResult> {
    let (blob_base_url, blob_store_path) =
        runtimed_client::daemon_paths::get_blob_paths_async(socket_path).await;
    let execution_id = record.execution_id.clone();
    let exec = runtime_doc::ExecutionState {
        cell_id: record.cell_id.unwrap_or_default(),
        status: record.status,
        execution_count: record.execution_count,
        success: record.success,
        outputs: record.outputs,
        source: record.source,
        seq: record.seq,
    };
    cell_result_from_execution_state(
        &execution_id,
        None,
        &exec,
        None,
        &blob_base_url,
        &blob_store_path,
    )
    .await
}

async fn cell_result_from_execution_state(
    execution_id: &str,
    fallback_cell_id: Option<&str>,
    state: &runtime_doc::ExecutionState,
    comms: Option<&CommMap>,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Result<CellResult> {
    let cell_id = if state.cell_id.is_empty() {
        fallback_cell_id.unwrap_or_default()
    } else {
        state.cell_id.as_str()
    };
    let success = state.success.unwrap_or(true);
    cell_result_from_output_manifests(
        ExecutionResultParts {
            execution_id,
            cell_id,
            status: &state.status,
            success,
            execution_count: state.execution_count,
            output_manifests: &state.outputs,
        },
        OutputResolutionContext {
            comms,
            blob_base_url,
            blob_store_path,
        },
    )
    .await
}

async fn cell_result_from_output_manifests(
    parts: ExecutionResultParts<'_>,
    context: OutputResolutionContext<'_>,
) -> Result<CellResult> {
    let resolved = shared_resolver::resolve_cell_outputs(
        parts.output_manifests,
        context.blob_base_url,
        context.blob_store_path,
        context.comms,
    )
    .await;
    let outputs: Vec<JsOutput> = resolved
        .into_iter()
        .map(to_js_output)
        .collect::<napi::Result<Vec<_>>>()?;
    let success = parts.status == "done"
        && parts.success
        && !outputs.iter().any(|o| o.output_type == "error");

    Ok(CellResult {
        cell_id: parts.cell_id.to_string(),
        execution_id: parts.execution_id.to_string(),
        execution_count: parts.execution_count,
        status: parts.status.to_string(),
        success,
        outputs,
    })
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
    use std::path::Path;

    #[test]
    fn resolve_socket_path_prefers_explicit_override() {
        let path = resolve_socket_path(Some("/tmp/runtimed-node-test.sock".to_string()));
        assert_eq!(path, PathBuf::from("/tmp/runtimed-node-test.sock"));
    }

    #[test]
    fn normalize_cell_type_defaults_and_validates() {
        assert_eq!(normalize_cell_type(None).unwrap(), "code");
        assert_eq!(
            normalize_cell_type(Some("markdown".to_string())).unwrap(),
            "markdown"
        );
        assert_eq!(normalize_cell_type(Some("raw".to_string())).unwrap(), "raw");

        let err = normalize_cell_type(Some("sql".to_string())).unwrap_err();
        assert!(err.reason.contains("Invalid cell_type"));
        assert!(err.reason.contains("code, markdown, raw"));
    }

    #[test]
    fn package_manager_converts_to_protocol_enum() {
        let uv: notebook_protocol::connection::PackageManager = PackageManager::Uv.into();
        let conda: notebook_protocol::connection::PackageManager = PackageManager::Conda.into();
        let pixi: notebook_protocol::connection::PackageManager = PackageManager::Pixi.into();

        assert_eq!(uv.as_str(), "uv");
        assert_eq!(conda.as_str(), "conda");
        assert_eq!(pixi.as_str(), "pixi");
    }

    #[test]
    fn execution_store_dir_falls_back_to_socket_namespace() {
        let dir =
            execution_store_dir_for_socket(Path::new("/tmp/runtimed-test/runtimed.sock"), None);

        assert_eq!(dir, PathBuf::from("/tmp/runtimed-test/executions"));
    }

    #[tokio::test]
    async fn execution_state_result_includes_execution_id_and_terminal_fields() {
        let state = runtime_doc::ExecutionState {
            cell_id: "cell-1".to_string(),
            status: "done".to_string(),
            execution_count: Some(3),
            success: Some(true),
            outputs: vec![],
            source: Some("1 + 1".to_string()),
            seq: Some(9),
        };

        let result = cell_result_from_execution_state("exec-1", None, &state, None, &None, &None)
            .await
            .unwrap();

        assert_eq!(result.cell_id, "cell-1");
        assert_eq!(result.execution_id, "exec-1");
        assert_eq!(result.status, "done");
        assert_eq!(result.execution_count, Some(3));
        assert!(result.success);
        assert!(result.outputs.is_empty());
    }

    #[tokio::test]
    async fn execution_record_result_recovers_required_execution_id() {
        let state = runtime_doc::ExecutionState {
            cell_id: "cell-from-record".to_string(),
            status: "done".to_string(),
            execution_count: Some(5),
            success: Some(true),
            outputs: vec![],
            source: Some("print('ok')".to_string()),
            seq: Some(11),
        };
        let record = runtimed_client::execution_store::ExecutionRecord::from_execution_state(
            "exec-from-store",
            "notebook",
            "nb-1",
            None,
            &state,
        );

        let result = cell_result_from_record(Path::new("/tmp/runtimed-node-test.sock"), record)
            .await
            .unwrap();

        assert_eq!(result.cell_id, "cell-from-record");
        assert_eq!(result.execution_id, "exec-from-store");
        assert_eq!(result.execution_count, Some(5));
        assert!(result.success);
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
