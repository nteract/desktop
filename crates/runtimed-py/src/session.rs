//! Synchronous Session for code execution.
//!
//! Thin wrapper around `session_core` async functions, using
//! `runtime.block_on()` to provide a synchronous Python API.
//! All business logic lives in `session_core.rs`.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use crate::error::to_py_err;
use crate::event_stream::ExecutionEventIterator;
use crate::output::{Cell, ExecutionResult, NotebookConnectionInfo, SyncEnvironmentResult};
use crate::session_core::{self, SessionState};
use crate::subscription::EventIteratorSubscription;

/// A session for executing code via the runtimed daemon.
///
/// Each session connects to a unique "virtual notebook" room in the daemon
/// and can launch a kernel and execute code. Sessions are isolated from
/// each other but multiple sessions can share the same kernel if they
/// use the same notebook_id.
///
/// Example:
///     session = Session()
///     session.start_kernel()
///     result = session.execute("print('hello')")
///     print(result.stdout)  # "hello\n"
#[pyclass]
pub struct Session {
    runtime: Runtime,
    state: Arc<Mutex<SessionState>>,
    notebook_id: String,
    /// Re-keyed notebook ID after saving an ephemeral room.
    /// Stored outside `SessionState` (behind a lightweight `std::sync::Mutex`)
    /// so the `notebook_id` getter never contends with the async `tokio::sync::Mutex`.
    notebook_id_override: Arc<std::sync::Mutex<Option<String>>>,
    peer_label: Option<String>,
}

impl Session {
    /// Open a notebook with a specific socket path (used by NativeClient).
    pub(crate) fn open_notebook_with_socket(
        socket_path: PathBuf,
        path: &str,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let runtime = Runtime::new().map_err(to_py_err)?;
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);

        let (notebook_id, mut state, _info) = runtime.block_on(session_core::connect_open(
            socket_path,
            path,
            actor_label.as_deref(),
        ))?;

        state.peer_label = peer_label.clone();

        // Keep the runtime alive — the sync task was spawned on it during
        // connect_open. Dropping the runtime would cancel the sync task and
        // break all subsequent daemon requests (start_kernel, execute, etc.).
        Self::from_state_with_runtime(runtime, notebook_id, state, peer_label)
    }

    /// Create a notebook without deprecation warning (used by Client).
    pub(crate) fn create_notebook_with_socket(
        socket_path: PathBuf,
        runtime_type: &str,
        working_dir: Option<&str>,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        if let Some(wd) = working_dir {
            let path = std::path::Path::new(wd);
            if !path.exists() {
                return Err(pyo3::exceptions::PyFileNotFoundError::new_err(format!(
                    "working_dir does not exist: {}",
                    wd
                )));
            }
            if !path.is_dir() {
                return Err(pyo3::exceptions::PyNotADirectoryError::new_err(format!(
                    "working_dir is not a directory: {}",
                    wd
                )));
            }
        }

        let runtime = Runtime::new().map_err(to_py_err)?;
        let working_dir_buf = working_dir.map(PathBuf::from);
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);

        let (notebook_id, mut state, _info) = runtime.block_on(session_core::connect_create(
            socket_path,
            runtime_type,
            working_dir_buf,
            actor_label.as_deref(),
        ))?;

        state.peer_label = peer_label.clone();

        Self::from_state_with_runtime(runtime, notebook_id, state, peer_label)
    }

    /// Join an existing notebook room by ID (used by Client).
    pub(crate) fn join_notebook_with_socket(
        socket_path: PathBuf,
        notebook_id: &str,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);

        let mut state = SessionState::new();
        state.peer_label = peer_label.clone();
        state.actor_label = actor_label;

        let runtime = Runtime::new().map_err(to_py_err)?;
        let state_arc = Arc::new(Mutex::new(state));
        runtime.block_on(session_core::connect_with_socket(
            &state_arc,
            notebook_id,
            socket_path,
        ))?;

        let state = Arc::try_unwrap(state_arc)
            .map_err(|_| to_py_err("Failed to unwrap session state"))?
            .into_inner();

        Self::from_state_with_runtime(runtime, notebook_id.to_string(), state, peer_label)
    }

    /// Create a pre-connected Session from a notebook_id and SessionState.
    /// Used by Client.open_notebook() / Client.create_notebook() / Client.join_notebook().
    /// Create a Session with a provided runtime.
    ///
    /// The runtime MUST be the same one that the sync task was spawned on
    /// during connect. Dropping that runtime kills the sync task and breaks
    /// all daemon communication.
    pub(crate) fn from_state_with_runtime(
        runtime: Runtime,
        notebook_id: String,
        state: SessionState,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let override_arc = Arc::new(std::sync::Mutex::new(None));
        if let Some(ref rx) = state.broadcast_rx {
            session_core::spawn_rekey_watcher(rx, Arc::clone(&override_arc), runtime.handle());
        }
        Ok(Self {
            runtime,
            state: Arc::new(Mutex::new(state)),
            notebook_id,
            notebook_id_override: override_arc,
            peer_label,
        })
    }
}

#[pymethods]
impl Session {
    /// The notebook ID for this session.
    /// After saving an ephemeral notebook, this reflects the new file-path ID.
    #[getter]
    fn notebook_id(&self) -> String {
        // If save() re-keyed the room, return the new file-path ID.
        // This lock is a std::sync::Mutex (not tokio), so it never contends
        // with async SessionState operations.
        if let Some(ref id) = *self.notebook_id_override.lock().unwrap() {
            return id.clone();
        }
        self.notebook_id.clone()
    }

    /// Whether the session is connected to the daemon.
    #[getter]
    fn is_connected(&self) -> bool {
        let state = self.runtime.block_on(self.state.lock());
        state.handle.is_some()
    }

    /// Whether a kernel has been started in this session.
    #[getter]
    fn kernel_started(&self) -> bool {
        let state = self.runtime.block_on(self.state.lock());
        state.kernel_started
    }

    /// Get the kernel type (e.g., "python", "deno") if kernel is running.
    #[getter]
    fn kernel_type(&self) -> Option<String> {
        let state = self.runtime.block_on(self.state.lock());
        state.kernel_type.clone()
    }

    /// Get the environment source (e.g., "uv:prewarmed") if kernel is running.
    #[getter]
    fn env_source(&self) -> Option<String> {
        let state = self.runtime.block_on(self.state.lock());
        state.env_source.clone()
    }

    /// Get connection info (from open_notebook/create_notebook).
    #[getter]
    fn connection_info(&self) -> Option<NotebookConnectionInfo> {
        let state = self.runtime.block_on(self.state.lock());
        state.connection_info.clone()
    }

    // =========================================================================
    // Connection
    // =========================================================================

    /// Connect to the daemon.
    fn connect(&self) -> PyResult<()> {
        let effective_id = self
            .notebook_id_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.notebook_id.clone());
        self.runtime
            .block_on(session_core::connect(&self.state, &effective_id))?;
        // Spawn background task to update notebook_id if a peer re-keys the room
        self.runtime.block_on(async {
            let st = self.state.lock().await;
            if let Some(ref rx) = st.broadcast_rx {
                session_core::spawn_rekey_watcher(
                    rx,
                    Arc::clone(&self.notebook_id_override),
                    self.runtime.handle(),
                );
            }
        });
        Ok(())
    }

    // =========================================================================
    // Kernel lifecycle
    // =========================================================================

    /// Start a kernel in the daemon.
    ///
    /// Args:
    ///     kernel_type: Type of kernel to start (default: "python").
    ///     env_source: Environment source (default: "auto").
    ///     notebook_path: Optional path for project file detection.
    ///
    /// Raises:
    ///     RuntimedError: If not connected or kernel launch fails.
    #[pyo3(signature = (kernel_type="python", env_source="auto", notebook_path=None))]
    fn start_kernel(
        &self,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&str>,
    ) -> PyResult<()> {
        self.connect()?;
        self.runtime.block_on(session_core::start_kernel(
            &self.state,
            kernel_type,
            env_source,
            notebook_path,
        ))
    }

    /// Shutdown the kernel.
    fn shutdown_kernel(&self) -> PyResult<()> {
        self.runtime
            .block_on(session_core::shutdown_kernel(&self.state))
    }

    /// Restart the kernel with auto environment detection.
    ///
    /// Args:
    ///     wait_for_ready: If True, wait for kernel to report idle (default: True).
    ///
    /// Returns:
    ///     List of progress messages emitted during environment preparation.
    #[pyo3(signature = (wait_for_ready=true))]
    fn restart_kernel(&self, wait_for_ready: bool) -> PyResult<Vec<String>> {
        self.runtime
            .block_on(session_core::restart_kernel(&self.state, wait_for_ready))
    }

    /// Interrupt the currently executing cell.
    fn interrupt(&self) -> PyResult<()> {
        self.runtime.block_on(session_core::interrupt(&self.state))
    }

    // =========================================================================
    // Cell operations
    // =========================================================================

    /// Create a new cell in the document (atomic: source set in same transaction).
    ///
    /// Args:
    ///     source: Cell source code (default: "").
    ///     cell_type: Cell type (default: "code").
    ///     index: Position to insert (default: end).
    ///
    /// Returns:
    ///     The cell ID (str).
    #[pyo3(signature = (source="", cell_type="code", index=None))]
    fn create_cell(&self, source: &str, cell_type: &str, index: Option<usize>) -> PyResult<String> {
        self.connect()?;
        self.runtime.block_on(session_core::create_cell(
            &self.state,
            source,
            cell_type,
            index,
        ))
    }

    /// Update a cell's source in the automerge document.
    fn set_source(&self, cell_id: &str, source: &str) -> PyResult<()> {
        self.runtime
            .block_on(session_core::set_source(&self.state, cell_id, source))
    }

    /// Splice a cell's source at a specific position (character-level, no diff).
    ///
    /// Deletes `delete_count` characters starting at `index`, then inserts `text`.
    /// This is the fast path for surgical edits — no Myers diff overhead.
    fn splice_source(
        &self,
        cell_id: &str,
        index: usize,
        delete_count: usize,
        text: &str,
    ) -> PyResult<()> {
        self.runtime.block_on(session_core::splice_source(
            &self.state,
            cell_id,
            index,
            delete_count,
            text,
        ))
    }

    /// Append text to a cell's source (efficient for streaming tokens).
    fn append_source(&self, cell_id: &str, text: &str) -> PyResult<()> {
        self.runtime
            .block_on(session_core::append_source(&self.state, cell_id, text))
    }

    /// Set a cell's type (code, markdown, or raw).
    fn set_cell_type(&self, cell_id: &str, cell_type: &str) -> PyResult<()> {
        self.runtime
            .block_on(session_core::set_cell_type(&self.state, cell_id, cell_type))
    }

    /// Get a cell by ID with resolved outputs.
    fn get_cell(&self, cell_id: &str) -> PyResult<Cell> {
        self.runtime
            .block_on(session_core::get_cell(&self.state, cell_id))
    }

    /// Get all cells with resolved outputs.
    fn get_cells(&self) -> PyResult<Vec<Cell>> {
        self.runtime.block_on(session_core::get_cells(&self.state))
    }

    /// Get a cell's source without materializing all cells.
    fn get_cell_source(&self, cell_id: &str) -> PyResult<Option<String>> {
        self.runtime
            .block_on(session_core::get_cell_source(&self.state, cell_id))
    }

    /// Get a cell's type without materializing all cells.
    fn get_cell_type(&self, cell_id: &str) -> PyResult<Option<String>> {
        self.runtime
            .block_on(session_core::get_cell_type(&self.state, cell_id))
    }

    /// Get a cell's raw outputs without blob resolution.
    fn get_cell_outputs(&self, cell_id: &str) -> PyResult<Option<Vec<String>>> {
        self.runtime
            .block_on(session_core::get_cell_outputs(&self.state, cell_id))
    }

    /// Get a cell's execution count.
    fn get_cell_execution_count(&self, cell_id: &str) -> PyResult<Option<String>> {
        self.runtime
            .block_on(session_core::get_cell_execution_count(&self.state, cell_id))
    }

    /// Get all cell IDs in document order.
    fn get_cell_ids(&self) -> PyResult<Vec<String>> {
        self.runtime
            .block_on(session_core::get_cell_ids(&self.state))
    }

    /// Get a cell's position (fractional index) without materializing all cells.
    fn get_cell_position(&self, cell_id: &str) -> PyResult<Option<String>> {
        self.runtime
            .block_on(session_core::get_cell_position(&self.state, cell_id))
    }

    /// Delete a cell from the document.
    fn delete_cell(&self, cell_id: &str) -> PyResult<()> {
        self.runtime
            .block_on(session_core::delete_cell(&self.state, cell_id))
    }

    /// Move a cell to after another cell (or to the beginning if None).
    #[pyo3(signature = (cell_id, after_cell_id=None))]
    fn move_cell(&self, cell_id: &str, after_cell_id: Option<&str>) -> PyResult<String> {
        self.runtime
            .block_on(session_core::move_cell(&self.state, cell_id, after_cell_id))
    }

    /// Clear a cell's outputs.
    fn clear_outputs(&self, cell_id: &str) -> PyResult<()> {
        self.runtime
            .block_on(session_core::clear_outputs(&self.state, cell_id))
    }

    // =========================================================================
    // Presence
    // =========================================================================

    /// Get all connected peer IDs and labels.
    ///
    /// Returns:
    ///     List of (peer_id, peer_label) tuples.
    fn get_peers(&self) -> PyResult<Vec<(String, String)>> {
        self.runtime.block_on(session_core::get_peers(&self.state))
    }

    /// Get remote peer cursors.
    ///
    /// Returns:
    ///     List of (peer_id, peer_label, cell_id, line, column) tuples.
    fn get_remote_cursors(&self) -> PyResult<Vec<session_core::RemoteCursor>> {
        self.runtime
            .block_on(session_core::get_remote_cursors(&self.state))
    }

    /// Set cursor position for collaborative presence.
    fn set_cursor(&self, cell_id: &str, line: u32, column: u32) -> PyResult<()> {
        self.runtime.block_on(session_core::set_cursor(
            &self.state,
            self.peer_label.as_deref(),
            cell_id,
            line,
            column,
        ))
    }

    /// Set selection range for collaborative presence.
    fn set_selection(
        &self,
        cell_id: &str,
        anchor_line: u32,
        anchor_col: u32,
        head_line: u32,
        head_col: u32,
    ) -> PyResult<()> {
        self.runtime.block_on(session_core::set_selection(
            &self.state,
            self.peer_label.as_deref(),
            cell_id,
            anchor_line,
            anchor_col,
            head_line,
            head_col,
        ))
    }

    /// Set cell focus (presence dot without cursor position).
    fn set_focus(&self, cell_id: &str) -> PyResult<()> {
        let state = self.state.clone();
        let cell_id = cell_id.to_string();
        let peer_label = self.peer_label.clone();
        self.runtime.block_on(async move {
            session_core::set_focus(&state, peer_label.as_deref(), &cell_id).await
        })
    }

    /// Clear cursor presence channel.
    fn clear_cursor(&self) -> PyResult<()> {
        let state = self.state.clone();
        self.runtime
            .block_on(async move { session_core::clear_cursor(&state).await })
    }

    /// Clear selection presence channel.
    fn clear_selection(&self) -> PyResult<()> {
        let state = self.state.clone();
        self.runtime
            .block_on(async move { session_core::clear_selection(&state).await })
    }

    // =========================================================================
    // Save / Metadata
    // =========================================================================

    /// Save the notebook to disk.
    ///
    /// Args:
    ///     path: Optional path override. If not provided, saves to original path.
    ///
    /// Returns:
    ///     The path the notebook was saved to.
    #[pyo3(signature = (path=None))]
    fn save(&mut self, path: Option<&str>) -> PyResult<String> {
        self.connect()?;
        let result = self
            .runtime
            .block_on(session_core::save(&self.state, path))?;
        // If the daemon re-keyed the room (ephemeral → file-path),
        // store the new ID so the notebook_id getter reflects it.
        if let Some(new_id) = result.new_notebook_id {
            *self.notebook_id_override.lock().unwrap() = Some(new_id);
        }
        Ok(result.path)
    }

    /// Set a notebook metadata key.
    fn set_metadata(&self, key: &str, value: &str) -> PyResult<()> {
        self.connect()?;
        self.runtime
            .block_on(session_core::set_metadata(&self.state, key, value))
    }

    /// Get a notebook metadata key.
    fn get_metadata(&self, key: &str) -> PyResult<Option<String>> {
        self.connect()?;
        self.runtime
            .block_on(session_core::get_metadata(&self.state, key))
    }

    /// Set the notebook kernelspec.
    #[pyo3(signature = (name, display_name, language=None))]
    fn set_kernelspec(
        &self,
        name: &str,
        display_name: &str,
        language: Option<&str>,
    ) -> PyResult<()> {
        self.connect()?;
        let mut snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        snapshot.kernelspec = Some(runtimed::notebook_metadata::KernelspecSnapshot {
            name: name.to_string(),
            display_name: display_name.to_string(),
            language: language.map(|s| s.to_string()),
        });
        self.runtime
            .block_on(session_core::set_notebook_metadata(&self.state, &snapshot))
    }

    /// Get the notebook kernelspec.
    ///
    /// Returns a dict with 'name', 'display_name', and optionally 'language',
    /// or None if no kernelspec is set.
    fn get_kernelspec(&self) -> PyResult<Option<std::collections::HashMap<String, String>>> {
        self.connect()?;
        let snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        Ok(snapshot.kernelspec.map(|ks| {
            let mut map = std::collections::HashMap::new();
            map.insert("name".to_string(), ks.name);
            map.insert("display_name".to_string(), ks.display_name);
            if let Some(lang) = ks.language {
                map.insert("language".to_string(), lang);
            }
            map
        }))
    }

    // =========================================================================
    // Cell metadata
    // =========================================================================

    /// Get cell metadata as a JSON string.
    fn get_cell_metadata(&self, cell_id: &str) -> PyResult<Option<String>> {
        self.runtime
            .block_on(session_core::get_cell_metadata(&self.state, cell_id))
    }

    /// Set cell metadata from a JSON string. Returns True on success.
    fn set_cell_metadata(&self, cell_id: &str, metadata_json: &str) -> PyResult<bool> {
        self.runtime.block_on(session_core::set_cell_metadata(
            &self.state,
            cell_id,
            metadata_json,
        ))
    }

    /// Update cell metadata at a specific path. Returns True on success.
    fn update_cell_metadata_at(
        &self,
        cell_id: &str,
        path: Vec<String>,
        value_json: &str,
    ) -> PyResult<bool> {
        self.runtime.block_on(session_core::update_cell_metadata_at(
            &self.state,
            cell_id,
            path,
            value_json,
        ))
    }

    /// Set whether a cell's source is hidden.
    fn set_cell_source_hidden(&self, cell_id: &str, hidden: bool) -> PyResult<bool> {
        let val = if hidden { "true" } else { "false" };
        self.update_cell_metadata_at(cell_id, vec!["jupyter".into(), "source_hidden".into()], val)
    }

    /// Set whether a cell's outputs are hidden.
    fn set_cell_outputs_hidden(&self, cell_id: &str, hidden: bool) -> PyResult<bool> {
        let val = if hidden { "true" } else { "false" };
        self.update_cell_metadata_at(
            cell_id,
            vec!["jupyter".into(), "outputs_hidden".into()],
            val,
        )
    }

    /// Set cell tags.
    fn set_cell_tags(&self, cell_id: &str, tags: Vec<String>) -> PyResult<bool> {
        let val = serde_json::to_string(&tags).map_err(|e| to_py_err(format!("JSON: {}", e)))?;
        self.update_cell_metadata_at(cell_id, vec!["tags".into()], &val)
    }

    // =========================================================================
    // Dependencies (uv / conda)
    // =========================================================================

    /// Get current UV dependencies.
    fn get_uv_dependencies(&self) -> PyResult<Vec<String>> {
        let snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        Ok(snapshot
            .runt
            .uv
            .map(|uv| uv.dependencies)
            .unwrap_or_default())
    }

    /// Add a UV dependency (deduplicates by package name).
    fn add_uv_dependency(&self, package: &str) -> PyResult<()> {
        let mut snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        snapshot.add_uv_dependency(package);
        self.runtime
            .block_on(session_core::set_notebook_metadata(&self.state, &snapshot))
    }

    /// Remove a UV dependency by package name. Returns True if removed.
    fn remove_uv_dependency(&self, package: &str) -> PyResult<bool> {
        let mut snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        let removed = snapshot.remove_uv_dependency(package);
        if removed {
            self.runtime
                .block_on(session_core::set_notebook_metadata(&self.state, &snapshot))?;
        }
        Ok(removed)
    }

    /// Get current Conda dependencies.
    fn get_conda_dependencies(&self) -> PyResult<Vec<String>> {
        let snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        Ok(snapshot
            .runt
            .conda
            .map(|c| c.dependencies)
            .unwrap_or_default())
    }

    /// Add a Conda dependency (deduplicates by package name).
    fn add_conda_dependency(&self, package: &str) -> PyResult<()> {
        let mut snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        snapshot.add_conda_dependency(package);
        self.runtime
            .block_on(session_core::set_notebook_metadata(&self.state, &snapshot))
    }

    /// Remove a Conda dependency by package name. Returns True if removed.
    fn remove_conda_dependency(&self, package: &str) -> PyResult<bool> {
        let mut snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        let removed = snapshot.remove_conda_dependency(package);
        if removed {
            self.runtime
                .block_on(session_core::set_notebook_metadata(&self.state, &snapshot))?;
        }
        Ok(removed)
    }

    /// Get the notebook's environment type from metadata structure.
    ///
    /// Returns "uv", "conda", or None if no env metadata exists.
    fn get_metadata_env_type(&self) -> PyResult<Option<String>> {
        let snapshot = self
            .runtime
            .block_on(session_core::get_notebook_metadata(&self.state))?;
        Ok(session_core::get_metadata_env_type(&snapshot))
    }

    /// Get user settings from local replica.
    ///
    /// Returns a dictionary with settings synced from daemon at connection time.
    /// Returns None if settings sync failed during connection.
    fn get_settings<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let state = self.runtime.block_on(self.state.lock());

        match session_core::get_settings(&state) {
            Some(settings) => {
                let dict = PyDict::new(py);
                dict.set_item("theme", settings.theme.to_string())?;
                dict.set_item("default_runtime", settings.default_runtime.to_string())?;
                dict.set_item(
                    "default_python_env",
                    settings.default_python_env.to_string(),
                )?;
                dict.set_item("keep_alive_secs", settings.keep_alive_secs)?;
                dict.set_item("onboarding_completed", settings.onboarding_completed)?;

                let uv_dict = PyDict::new(py);
                uv_dict.set_item("default_packages", &settings.uv.default_packages)?;
                dict.set_item("uv", uv_dict)?;

                let conda_dict = PyDict::new(py);
                conda_dict.set_item("default_packages", &settings.conda.default_packages)?;
                dict.set_item("conda", conda_dict)?;

                Ok(Some(dict))
            }
            None => Ok(None),
        }
    }

    // =========================================================================
    // Execution
    // =========================================================================

    /// Execute a cell by ID.
    ///
    /// The entire lifecycle (confirm_sync, send_request, collect_outputs)
    /// is wrapped in a single timeout.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///     timeout_secs: Maximum time to wait for execution (default: 60).
    ///
    /// Returns:
    ///     ExecutionResult with outputs, success status, and execution count.
    ///
    /// Raises:
    ///     RuntimedError: If not connected, cell not found, or timeout.
    #[pyo3(signature = (cell_id, timeout_secs=60.0))]
    fn execute_cell(&self, cell_id: &str, timeout_secs: f64) -> PyResult<ExecutionResult> {
        self.runtime.block_on(session_core::execute_cell(
            &self.state,
            &self.notebook_id,
            cell_id,
            timeout_secs,
        ))
    }

    /// Create a cell, execute it, and return the result.
    ///
    /// Convenience method that combines create_cell + execute_cell.
    ///
    /// Args:
    ///     code: The code to execute.
    ///     timeout_secs: Maximum time to wait (default: 60).
    ///
    /// Returns:
    ///     ExecutionResult with outputs, success status, and execution count.
    #[pyo3(signature = (code, timeout_secs=60.0))]
    fn run(&self, code: &str, timeout_secs: f64) -> PyResult<ExecutionResult> {
        self.runtime.block_on(session_core::run(
            &self.state,
            &self.notebook_id,
            code,
            timeout_secs,
        ))
    }

    /// Queue a cell for execution without waiting for the result.
    fn queue_cell(&self, cell_id: &str) -> PyResult<()> {
        self.runtime.block_on(session_core::queue_cell(
            &self.state,
            &self.notebook_id,
            cell_id,
        ))
    }

    /// Stream execution events for a cell as an iterator.
    ///
    /// Unlike execute_cell() which blocks until completion and returns all
    /// outputs at once, this returns an iterator that yields ExecutionEvent
    /// objects as they arrive from the kernel, enabling real-time processing.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///     timeout_secs: Maximum time to wait for each event (default: 60).
    ///     signal_only: If True, output events contain only output_index, not
    ///         the full output data. Use get_cell() to fetch the data.
    ///
    /// Returns:
    ///     ExecutionEventIterator that yields ExecutionEvent objects.
    #[pyo3(signature = (cell_id, timeout_secs=60.0, signal_only=false))]
    fn stream_execute(
        &self,
        cell_id: &str,
        timeout_secs: f64,
        signal_only: bool,
    ) -> PyResult<ExecutionEventIterator> {
        let (broadcast_rx, blob_base_url, blob_store_path) = self.runtime.block_on(
            session_core::prepare_stream_execute(&self.state, &self.notebook_id, cell_id),
        )?;

        ExecutionEventIterator::new(
            broadcast_rx,
            cell_id.to_string(),
            timeout_secs,
            blob_base_url,
            blob_store_path,
            signal_only,
        )
    }

    /// Subscribe to execution events for specific cells or event types.
    ///
    /// Returns a sync iterator subscription that yields events as they arrive.
    ///
    /// Args:
    ///     cell_ids: Optional list of cell IDs to filter (None = all cells).
    ///     event_types: Optional list of event types to filter (None = all types).
    #[pyo3(signature = (cell_ids=None, event_types=None))]
    fn subscribe(
        &self,
        cell_ids: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
    ) -> PyResult<EventIteratorSubscription> {
        let (broadcast_rx, blob_base_url, blob_store_path) = self
            .runtime
            .block_on(session_core::prepare_subscribe(&self.state))?;

        EventIteratorSubscription::new(
            broadcast_rx,
            cell_ids,
            event_types,
            blob_base_url,
            blob_store_path,
        )
    }

    // =========================================================================
    // Environment sync
    // =========================================================================

    /// Sync environment with current notebook metadata.
    ///
    /// Returns:
    ///     SyncEnvironmentResult with success status and installed packages.
    fn sync_environment(&self) -> PyResult<SyncEnvironmentResult> {
        self.runtime
            .block_on(session_core::sync_environment_impl(&self.state))
    }

    // =========================================================================
    // Completion, history, queue
    // =========================================================================

    /// Get code completions at the given cursor position.
    ///
    /// Args:
    ///     code: The code buffer to complete in.
    ///     cursor_pos: Cursor position (byte offset) in the code.
    ///
    /// Returns:
    ///     CompletionResult with items, cursor_start, and cursor_end.
    fn complete(
        &self,
        code: String,
        cursor_pos: usize,
    ) -> PyResult<crate::output::CompletionResult> {
        self.runtime
            .block_on(session_core::complete(&self.state, &code, cursor_pos))
    }

    /// Get execution history from the kernel.
    ///
    /// Args:
    ///     pattern: Optional glob pattern to filter history entries.
    ///     n: Maximum number of entries to return (default: 100).
    ///     unique: If True, deduplicate entries (default: True).
    #[pyo3(signature = (pattern=None, n=100, unique=true))]
    fn get_history(
        &self,
        pattern: Option<String>,
        n: i32,
        unique: bool,
    ) -> PyResult<Vec<crate::output::HistoryEntry>> {
        self.runtime.block_on(session_core::get_history(
            &self.state,
            pattern.as_deref(),
            n,
            unique,
        ))
    }

    /// Get the current execution queue state.
    fn get_queue_state(&self) -> PyResult<crate::output::QueueState> {
        self.runtime
            .block_on(session_core::get_queue_state(&self.state))
    }

    /// Get the full runtime state from the daemon's RuntimeStateDoc.
    ///
    /// Returns kernel status, execution queue, environment sync state,
    /// and last-saved timestamp — all read from the local Automerge
    /// replica (no daemon round-trip).
    fn get_runtime_state(&self) -> PyResult<crate::output::PyRuntimeState> {
        self.runtime
            .block_on(session_core::get_runtime_state(&self.state))
    }

    /// Execute all code cells in document order. Returns number of cells queued.
    fn run_all_cells(&self) -> PyResult<usize> {
        self.runtime
            .block_on(session_core::run_all_cells(&self.state, &self.notebook_id))
    }

    // =========================================================================
    // Low-level sync (for testing / cross-impl verification)
    // =========================================================================

    /// Get the raw Automerge document bytes from the local replica.
    ///
    /// Returns the full serialized document as `bytes`. Useful for
    /// cross-implementation testing (e.g., loading WASM-side).
    fn get_automerge_doc_bytes<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
        let bytes = self
            .runtime
            .block_on(session_core::get_automerge_doc_bytes(&self.state))?;
        Ok(pyo3::types::PyBytes::new(py, &bytes))
    }

    /// Confirm that the daemon has merged all local changes.
    ///
    /// Blocks until the daemon acknowledges our local heads. Called
    /// internally by execute_cell, but exposed for tests that need
    /// an explicit sync barrier.
    fn confirm_sync(&self) -> PyResult<()> {
        self.runtime
            .block_on(session_core::confirm_sync(&self.state))
    }

    // =========================================================================
    // Repr, context manager, close
    // =========================================================================

    fn __repr__(&self) -> String {
        let state = self.runtime.block_on(self.state.lock());
        let status = if state.kernel_started {
            "kernel_running"
        } else if state.handle.is_some() {
            "connected"
        } else {
            "disconnected"
        };
        format!("Session(id={}, status={})", self.notebook_id, status)
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        Ok(false)
    }

    /// Close the session.
    ///
    /// Does NOT shutdown the kernel - the daemon handles kernel lifecycle
    /// based on peer count. When all peers disconnect, the daemon will
    /// clean up the kernel. Use shutdown_kernel() explicitly if you need
    /// to stop the kernel.
    fn close(&self) -> PyResult<()> {
        let mut st = self.runtime.block_on(self.state.lock());
        st.handle = None;
        st.broadcast_rx = None;
        Ok(())
    }
}
