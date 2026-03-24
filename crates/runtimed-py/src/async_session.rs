//! Async Session for code execution.
//!
//! Thin wrapper around `session_core` async functions, using
//! `future_into_py()` to provide an async Python API.
//! All business logic lives in `session_core.rs`.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_async_runtimes::tokio::future_into_py;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::to_py_err;
use crate::output::{Cell, PyRuntimeState};
use crate::session_core::{self, SessionState};

/// An async session for executing code via the runtimed daemon.
///
/// Each session connects to a unique "virtual notebook" room in the daemon
/// and can launch a kernel and execute code. Sessions are isolated from
/// each other but multiple sessions can share the same kernel if they
/// use the same notebook_id.
///
/// Example:
///     async with AsyncSession() as session:
///         await session.start_kernel()
///         cell_id = await session.create_cell("print('hello')")
///         result = await session.execute_cell(cell_id)
///         print(result.stdout)  # "hello\n"
#[pyclass]
pub struct AsyncSession {
    state: Arc<Mutex<SessionState>>,
    notebook_id: String,
    /// Re-keyed notebook ID after saving an ephemeral room.
    /// Stored outside `SessionState` (behind a lightweight `std::sync::Mutex`)
    /// so the `notebook_id` getter never contends with the async `tokio::sync::Mutex`.
    notebook_id_override: Arc<std::sync::Mutex<Option<String>>>,
    peer_label: Option<String>,
}

impl AsyncSession {
    /// Create a pre-connected AsyncSession from a notebook_id and SessionState.
    /// Used by AsyncClient.open_notebook() / AsyncClient.create_notebook() / AsyncClient.join_notebook().
    pub(crate) fn from_state(
        notebook_id: String,
        state: SessionState,
        peer_label: Option<String>,
    ) -> Self {
        let override_arc = Arc::new(std::sync::Mutex::new(None));
        if let Some(ref rx) = state.broadcast_rx {
            session_core::spawn_rekey_watcher(
                rx,
                Arc::clone(&override_arc),
                &tokio::runtime::Handle::current(),
            );
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            notebook_id,
            notebook_id_override: override_arc,
            peer_label,
        }
    }

    /// Async helper for open_notebook (no deprecation warning).
    pub(crate) async fn open_notebook_async(
        socket_path: PathBuf,
        path: String,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let peer_label = Some(peer_label.unwrap_or_else(session_core::default_peer_label));
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);
        let (notebook_id, mut state, _info) =
            session_core::connect_open(socket_path, &path, actor_label.as_deref()).await?;
        state.peer_label = peer_label.clone();
        session_core::announce_presence(&state).await;
        Ok(Self::from_state(notebook_id, state, peer_label))
    }

    /// Async helper for create_notebook (no deprecation warning).
    pub(crate) async fn create_notebook_async(
        socket_path: PathBuf,
        runtime: String,
        working_dir: Option<PathBuf>,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let peer_label = Some(peer_label.unwrap_or_else(session_core::default_peer_label));
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);
        let (notebook_id, mut state, _info) = session_core::connect_create(
            socket_path,
            &runtime,
            working_dir,
            actor_label.as_deref(),
        )
        .await?;
        state.peer_label = peer_label.clone();
        session_core::announce_presence(&state).await;
        Ok(Self::from_state(notebook_id, state, peer_label))
    }

    /// Async helper for join_notebook (no deprecation warning).
    pub(crate) async fn join_notebook_async(
        socket_path: PathBuf,
        notebook_id: String,
        peer_label: Option<String>,
    ) -> PyResult<Self> {
        let peer_label = Some(peer_label.unwrap_or_else(session_core::default_peer_label));
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);
        let mut state = SessionState::new();
        state.peer_label = peer_label.clone();
        state.actor_label = actor_label;

        let state_arc = Arc::new(Mutex::new(state));
        session_core::connect_with_socket(&state_arc, &notebook_id, socket_path).await?;

        let state = Arc::try_unwrap(state_arc)
            .map_err(|_| to_py_err("Failed to unwrap session state"))?
            .into_inner();
        session_core::announce_presence(&state).await;

        Ok(Self::from_state(notebook_id, state, peer_label))
    }
}

#[pymethods]
impl AsyncSession {
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
    fn is_connected<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let st = state.lock().await;
            Ok(st.handle.is_some())
        })
    }

    /// Whether a kernel has been started.
    fn kernel_started<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let st = state.lock().await;
            Ok(st.kernel_started)
        })
    }

    /// Get the kernel type (e.g., "python", "deno") if kernel is running.
    fn kernel_type<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let st = state.lock().await;
            Ok(st.kernel_type.clone())
        })
    }

    /// Get the environment source if kernel is running.
    fn env_source<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let st = state.lock().await;
            Ok(st.env_source.clone())
        })
    }

    /// Get connection info (from open_notebook/create_notebook).
    fn connection_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let st = state.lock().await;
            Ok(st.connection_info.clone())
        })
    }

    // =========================================================================
    // Connection
    // =========================================================================

    /// Connect to the daemon.
    fn connect<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let override_arc = Arc::clone(&self.notebook_id_override);
        let effective_id = self
            .notebook_id_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.notebook_id.clone());
        future_into_py(py, async move {
            session_core::connect(&state, &effective_id).await?;
            // Announce presence so the daemon registers this peer immediately
            {
                let st = state.lock().await;
                session_core::announce_presence(&st).await;
            }
            // Spawn background task to update notebook_id if a peer re-keys the room
            {
                let st = state.lock().await;
                if let Some(ref rx) = st.broadcast_rx {
                    session_core::spawn_rekey_watcher(
                        rx,
                        override_arc,
                        &tokio::runtime::Handle::current(),
                    );
                }
            }
            Ok(())
        })
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
    #[pyo3(signature = (kernel_type="python", env_source="auto", notebook_path=None))]
    fn start_kernel<'py>(
        &self,
        py: Python<'py>,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let kernel_type = kernel_type.to_string();
        let env_source = env_source.to_string();
        let notebook_path = notebook_path.map(|s| s.to_string());

        future_into_py(py, async move {
            // Ensure connected first
            session_core::connect(&state, &notebook_id).await?;
            session_core::start_kernel(&state, &kernel_type, &env_source, notebook_path.as_deref())
                .await
        })
    }

    /// Shutdown the kernel.
    fn shutdown_kernel<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(
            py,
            async move { session_core::shutdown_kernel(&state).await },
        )
    }

    /// Restart the kernel with auto environment detection.
    ///
    /// Args:
    ///     wait_for_ready: If True, wait for kernel to report idle (default: True).
    ///
    /// Returns:
    ///     List of progress messages emitted during environment preparation.
    #[pyo3(signature = (wait_for_ready=true))]
    fn restart_kernel<'py>(
        &self,
        py: Python<'py>,
        wait_for_ready: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            session_core::restart_kernel(&state, wait_for_ready).await
        })
    }

    /// Interrupt the currently executing cell.
    fn interrupt<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move { session_core::interrupt(&state).await })
    }

    // =========================================================================
    // Cell operations
    // =========================================================================

    /// Create a new cell in the document (atomic: source set in same transaction).
    ///
    /// Returns a coroutine that resolves to the cell ID (str).
    #[pyo3(signature = (source="", cell_type="code", index=None))]
    fn create_cell<'py>(
        &self,
        py: Python<'py>,
        source: &str,
        cell_type: &str,
        index: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let source = source.to_string();
        let cell_type = cell_type.to_string();

        future_into_py(py, async move {
            session_core::create_cell(&state, &source, &cell_type, index).await
        })
    }

    /// Update a cell's source in the automerge document.
    fn set_source<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        source: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let source = source.to_string();

        future_into_py(py, async move {
            session_core::set_source(&state, &cell_id, &source).await
        })
    }

    /// Splice a cell's source at a specific position (character-level, no diff).
    ///
    /// Deletes `delete_count` characters starting at `index`, then inserts `text`.
    /// This is the fast path for surgical edits — no Myers diff overhead.
    ///
    /// Args:
    ///     cell_id: The cell to edit.
    ///     index: Character index where the splice starts.
    ///     delete_count: Number of characters to delete at index.
    ///     text: Text to insert at index (after deletion).
    fn splice_source<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        index: usize,
        delete_count: usize,
        text: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let text = text.to_string();

        future_into_py(py, async move {
            session_core::splice_source(&state, &cell_id, index, delete_count, &text).await
        })
    }

    /// Append text to a cell's source (efficient for streaming tokens).
    fn append_source<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        text: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let text = text.to_string();

        future_into_py(py, async move {
            session_core::append_source(&state, &cell_id, &text).await
        })
    }

    /// Set a cell's type (code, markdown, or raw).
    fn set_cell_type<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        cell_type: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let cell_type = cell_type.to_string();

        future_into_py(py, async move {
            session_core::set_cell_type(&state, &cell_id, &cell_type).await
        })
    }

    /// Get a cell by ID with resolved outputs.
    fn get_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();

        future_into_py(
            py,
            async move { session_core::get_cell(&state, &cell_id).await },
        )
    }

    /// Get all cells with resolved outputs.
    fn get_cells<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move { session_core::get_cells(&state).await })
    }

    /// Get a cell's source without materializing all cells.
    fn get_cell_source<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_source(&state, &cell_id).await
        })
    }

    /// Get a cell's type without materializing all cells.
    fn get_cell_type<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_type(&state, &cell_id).await
        })
    }

    /// Get a cell's raw outputs without blob resolution.
    fn get_cell_outputs<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_outputs(&state, &cell_id).await
        })
    }

    /// Get a cell's execution count without materializing all cells.
    fn get_cell_execution_count<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_execution_count(&state, &cell_id).await
        })
    }

    /// Get all cell IDs in document order.
    fn get_cell_ids<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move { session_core::get_cell_ids(&state).await })
    }

    /// Get a cell's position (fractional index) without materializing all cells.
    fn get_cell_position<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_position(&state, &cell_id).await
        })
    }

    /// Delete a cell from the document.
    fn delete_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::delete_cell(&state, &cell_id).await
        })
    }

    /// Move a cell to after another cell (or to the beginning if None).
    #[pyo3(signature = (cell_id, after_cell_id=None))]
    fn move_cell<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let after_cell_id = after_cell_id.map(|s| s.to_string());

        future_into_py(py, async move {
            session_core::move_cell(&state, &cell_id, after_cell_id.as_deref()).await
        })
    }

    /// Clear a cell's outputs.
    fn clear_outputs<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::clear_outputs(&state, &cell_id).await
        })
    }

    // =========================================================================
    // Presence
    // =========================================================================

    /// Get all connected peer IDs and labels.
    ///
    /// Returns:
    ///     List of (peer_id, peer_label) tuples.
    fn get_peers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move { session_core::get_peers(&state).await })
    }

    /// Get remote peer cursors.
    ///
    /// Returns:
    ///     List of (peer_id, peer_label, cell_id, line, column) tuples.
    fn get_remote_cursors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(
            py,
            async move { session_core::get_remote_cursors(&state).await },
        )
    }

    /// Set cursor position for collaborative presence.
    fn set_cursor<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        line: u32,
        column: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let peer_label = self.peer_label.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            session_core::set_cursor(&state, peer_label.as_deref(), &cell_id, line, column).await
        })
    }

    /// Set selection range for collaborative presence.
    fn set_selection<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        anchor_line: u32,
        anchor_col: u32,
        head_line: u32,
        head_col: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let peer_label = self.peer_label.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            session_core::set_selection(
                &state,
                peer_label.as_deref(),
                &cell_id,
                anchor_line,
                anchor_col,
                head_line,
                head_col,
            )
            .await
        })
    }

    /// Set cell focus (presence dot without cursor position).
    fn set_focus<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let peer_label = self.peer_label.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            session_core::set_focus(&state, peer_label.as_deref(), &cell_id).await
        })
    }

    /// Clear cursor presence channel.
    fn clear_cursor<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move { session_core::clear_cursor(&state).await })
    }

    /// Clear selection presence channel.
    fn clear_selection<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(
            py,
            async move { session_core::clear_selection(&state).await },
        )
    }

    // =========================================================================
    // Save / Metadata
    // =========================================================================

    /// Save the notebook to disk.
    ///
    /// Args:
    ///     path: Optional path override. If not provided, saves to original path.
    ///
    /// Returns a coroutine that resolves to the saved path (str).
    #[pyo3(signature = (path=None))]
    fn save<'py>(&self, py: Python<'py>, path: Option<&str>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let override_arc = Arc::clone(&self.notebook_id_override);
        let effective_id = self
            .notebook_id_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.notebook_id.clone());
        let path = path.map(|s| s.to_string());

        future_into_py(py, async move {
            session_core::connect(&state, &effective_id).await?;
            let result = session_core::save(&state, path.as_deref()).await?;
            // If the daemon re-keyed the room (ephemeral → file-path),
            // store the new ID so the notebook_id getter reflects it.
            if let Some(ref new_id) = result.new_notebook_id {
                *override_arc.lock().unwrap() = Some(new_id.clone());
            }
            Ok(result.path)
        })
    }

    /// Set a notebook metadata key.
    fn set_metadata<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        value: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let key = key.to_string();
        let value = value.to_string();

        future_into_py(py, async move {
            session_core::connect(&state, &notebook_id).await?;
            session_core::set_metadata(&state, &key, &value).await
        })
    }

    /// Get a notebook metadata key.
    fn get_metadata<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let key = key.to_string();

        future_into_py(py, async move {
            session_core::connect(&state, &notebook_id).await?;
            session_core::get_metadata(&state, &key).await
        })
    }

    /// Set the notebook kernelspec.
    #[pyo3(signature = (name, display_name, language=None))]
    fn set_kernelspec<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        display_name: &str,
        language: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let name = name.to_string();
        let display_name = display_name.to_string();
        let language = language.map(|s| s.to_string());

        future_into_py(py, async move {
            session_core::connect(&state, &notebook_id).await?;
            let mut snapshot = session_core::get_notebook_metadata(&state).await?;
            snapshot.kernelspec = Some(runtimed::notebook_metadata::KernelspecSnapshot {
                name,
                display_name,
                language,
            });
            session_core::set_notebook_metadata(&state, &snapshot).await
        })
    }

    /// Get the notebook kernelspec.
    ///
    /// Returns a dict with 'name', 'display_name', and optionally 'language',
    /// or None if no kernelspec is set.
    fn get_kernelspec<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();

        future_into_py(py, async move {
            session_core::connect(&state, &notebook_id).await?;
            let snapshot = session_core::get_notebook_metadata(&state).await?;
            Ok(snapshot.kernelspec.map(|ks| {
                let mut map = std::collections::HashMap::<String, String>::new();
                map.insert("name".to_string(), ks.name);
                map.insert("display_name".to_string(), ks.display_name);
                if let Some(lang) = ks.language {
                    map.insert("language".to_string(), lang);
                }
                map
            }))
        })
    }

    // =========================================================================
    // Cell metadata
    // =========================================================================

    /// Get cell metadata as a JSON string.
    fn get_cell_metadata<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::get_cell_metadata(&state, &cell_id).await
        })
    }

    /// Set cell metadata from a JSON string. Returns True on success.
    fn set_cell_metadata<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        metadata_json: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let metadata_json = metadata_json.to_string();
        future_into_py(py, async move {
            session_core::set_cell_metadata(&state, &cell_id, &metadata_json).await
        })
    }

    /// Update cell metadata at a specific path. Returns True on success.
    fn update_cell_metadata_at<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        path: Vec<String>,
        value_json: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let value_json = value_json.to_string();
        future_into_py(py, async move {
            session_core::update_cell_metadata_at(&state, &cell_id, path, &value_json).await
        })
    }

    /// Set whether a cell's source is hidden.
    fn set_cell_source_hidden<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        hidden: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let val = if hidden { "true" } else { "false" };
        let val = val.to_string();
        future_into_py(py, async move {
            session_core::update_cell_metadata_at(
                &state,
                &cell_id,
                vec!["jupyter".into(), "source_hidden".into()],
                &val,
            )
            .await
        })
    }

    /// Set whether a cell's outputs are hidden.
    fn set_cell_outputs_hidden<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        hidden: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let val = if hidden { "true" } else { "false" };
        let val = val.to_string();
        future_into_py(py, async move {
            session_core::update_cell_metadata_at(
                &state,
                &cell_id,
                vec!["jupyter".into(), "outputs_hidden".into()],
                &val,
            )
            .await
        })
    }

    /// Set cell tags.
    fn set_cell_tags<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        tags: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let val = serde_json::to_string(&tags).map_err(|e| to_py_err(format!("JSON: {}", e)))?;
        future_into_py(py, async move {
            session_core::update_cell_metadata_at(&state, &cell_id, vec!["tags".into()], &val).await
        })
    }

    // =========================================================================
    // Dependencies (uv / conda)
    // =========================================================================

    /// Get current UV dependencies.
    fn get_uv_dependencies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let snapshot = session_core::get_notebook_metadata(&state).await?;
            Ok(snapshot
                .runt
                .uv
                .map(|uv| uv.dependencies)
                .unwrap_or_default())
        })
    }

    /// Add a UV dependency (deduplicates by package name).
    fn add_uv_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();
        future_into_py(py, async move {
            let mut snapshot = session_core::get_notebook_metadata(&state).await?;
            snapshot.add_uv_dependency(&package);
            session_core::set_notebook_metadata(&state, &snapshot).await
        })
    }

    /// Remove a UV dependency by package name. Returns True if removed.
    fn remove_uv_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();
        future_into_py(py, async move {
            let mut snapshot = session_core::get_notebook_metadata(&state).await?;
            let removed = snapshot.remove_uv_dependency(&package);
            if removed {
                session_core::set_notebook_metadata(&state, &snapshot).await?;
            }
            Ok(removed)
        })
    }

    /// Get current Conda dependencies.
    fn get_conda_dependencies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let snapshot = session_core::get_notebook_metadata(&state).await?;
            Ok(snapshot
                .runt
                .conda
                .map(|c| c.dependencies)
                .unwrap_or_default())
        })
    }

    /// Add a Conda dependency (deduplicates by package name).
    fn add_conda_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();
        future_into_py(py, async move {
            let mut snapshot = session_core::get_notebook_metadata(&state).await?;
            snapshot.add_conda_dependency(&package);
            session_core::set_notebook_metadata(&state, &snapshot).await
        })
    }

    /// Remove a Conda dependency by package name. Returns True if removed.
    fn remove_conda_dependency<'py>(
        &self,
        py: Python<'py>,
        package: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let package = package.to_string();
        future_into_py(py, async move {
            let mut snapshot = session_core::get_notebook_metadata(&state).await?;
            let removed = snapshot.remove_conda_dependency(&package);
            if removed {
                session_core::set_notebook_metadata(&state, &snapshot).await?;
            }
            Ok(removed)
        })
    }

    /// Get the notebook's environment type from metadata structure.
    ///
    /// Returns "uv", "conda", or None if no env metadata exists.
    /// This checks if the metadata structure exists, not whether deps are non-empty.
    fn get_metadata_env_type<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let snapshot = session_core::get_notebook_metadata(&state).await?;
            Ok(session_core::get_metadata_env_type(&snapshot))
        })
    }

    /// Get user settings from local replica.
    ///
    /// Returns a dictionary with settings synced from daemon at connection time.
    /// Returns None if settings sync failed during connection.
    fn get_settings<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let state = self.state.blocking_lock();

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
    ///     timeout_secs: Maximum time to wait (default: 60).
    ///
    /// Returns a coroutine that resolves to ExecutionResult.
    #[pyo3(signature = (cell_id, timeout_secs=60.0))]
    fn execute_cell<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            session_core::execute_cell(&state, &notebook_id, &cell_id, timeout_secs).await
        })
    }

    /// Queue a cell for execution without waiting for the result.
    fn queue_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::queue_cell(&state, &notebook_id, &cell_id).await
        })
    }

    /// Wait for an already-queued execution to complete and return its outputs.
    ///
    /// Unlike execute_cell(), this does NOT re-queue the cell. Use this when
    /// you already have an execution_id from a prior queue_cell() call.
    ///
    /// Args:
    ///     cell_id: The cell ID being executed.
    ///     execution_id: The execution_id from queue_cell().
    ///     timeout_secs: Maximum time to wait (default: 60).
    #[pyo3(signature = (cell_id, execution_id, timeout_secs=60.0))]
    fn wait_for_execution<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        execution_id: &str,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        let execution_id = execution_id.to_string();

        future_into_py(py, async move {
            session_core::wait_for_execution(&state, &cell_id, &execution_id, timeout_secs).await
        })
    }

    // =========================================================================
    // Environment sync
    // =========================================================================

    /// Sync environment with current notebook metadata.
    ///
    /// Returns a coroutine that resolves to SyncEnvironmentResult.
    fn sync_environment<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            session_core::sync_environment_impl(&state).await
        })
    }

    // =========================================================================
    // Completion, history, queue
    // =========================================================================

    /// Get code completions at the given cursor position.
    fn complete<'py>(
        &self,
        py: Python<'py>,
        code: String,
        cursor_pos: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            session_core::complete(&state, &code, cursor_pos).await
        })
    }

    /// Get execution history from the kernel.
    #[pyo3(signature = (pattern=None, n=100, unique=true))]
    fn get_history<'py>(
        &self,
        py: Python<'py>,
        pattern: Option<String>,
        n: i32,
        unique: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            session_core::get_history(&state, pattern.as_deref(), n, unique).await
        })
    }

    /// Get the full runtime state from the daemon's RuntimeStateDoc.
    ///
    /// Returns kernel status, execution queue, environment sync state,
    /// and last-saved timestamp — all read from the local Automerge
    /// replica (no daemon round-trip).
    fn get_runtime_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(
            py,
            async move { session_core::get_runtime_state(&state).await },
        )
    }

    /// Get the current execution queue state.
    fn get_queue_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(
            py,
            async move { session_core::get_queue_state(&state).await },
        )
    }

    /// Execute all code cells in document order. Returns number of cells queued.
    fn run_all_cells<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        future_into_py(py, async move {
            session_core::run_all_cells(&state, &notebook_id).await
        })
    }

    // =========================================================================
    // Low-level sync (for testing / cross-impl verification)
    // =========================================================================

    /// Get the raw Automerge document bytes from the local replica.
    fn get_automerge_doc_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            session_core::get_automerge_doc_bytes(&state).await
        })
    }

    /// Confirm that the daemon has merged all local changes.
    fn confirm_sync<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move { session_core::confirm_sync(&state).await })
    }

    // =========================================================================
    // Synchronous reads (for Python wrapper's sync properties)
    // =========================================================================
    // These use blocking_lock() to read directly from the local Automerge
    // replica without going through future_into_py. Safe because Python
    // calls these from the main thread, not from the tokio runtime.

    /// Get all cell IDs in document order (sync read from local doc).
    fn get_cell_ids_sync(&self) -> PyResult<Vec<String>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        Ok(handle.get_cell_ids())
    }

    /// Get a cell's source (sync read from local doc).
    fn get_cell_source_sync(&self, cell_id: &str) -> PyResult<Option<String>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        Ok(handle.get_cell_source(cell_id))
    }

    /// Get a cell's type (sync read from local doc).
    fn get_cell_type_sync(&self, cell_id: &str) -> PyResult<Option<String>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        Ok(handle.get_cell_type(cell_id))
    }

    /// Get a cell's execution count (sync read from local doc).
    fn get_cell_execution_count_sync(&self, cell_id: &str) -> PyResult<Option<String>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        Ok(handle.get_cell_execution_count(cell_id))
    }

    /// Get a cell by ID with resolved outputs (sync — uses a temporary runtime for blob I/O).
    fn get_cell_sync(&self, cell_id: &str) -> PyResult<Cell> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        // Spawn a blocking task with a temporary runtime for the async output resolution.
        let rt = tokio::runtime::Runtime::new().map_err(to_py_err)?;
        rt.block_on(session_core::get_cell(&state, &cell_id))
    }

    /// Get all cells with resolved outputs (sync — uses a temporary runtime for blob I/O).
    fn get_cells_sync(&self) -> PyResult<Vec<Cell>> {
        let state = Arc::clone(&self.state);
        let rt = tokio::runtime::Runtime::new().map_err(to_py_err)?;
        rt.block_on(session_core::get_cells(&state))
    }

    /// Get cell metadata as JSON string (sync read from local doc).
    fn get_cell_metadata_sync(&self, cell_id: &str) -> PyResult<Option<String>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        match handle.get_cell_metadata(cell_id) {
            Some(metadata) => Ok(Some(
                serde_json::to_string(&metadata)
                    .map_err(|e| to_py_err(format!("Serialize: {}", e)))?,
            )),
            None => Ok(None),
        }
    }

    /// Get runtime state (sync read from local RuntimeStateDoc).
    fn get_runtime_state_sync(&self) -> PyResult<PyRuntimeState> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        let rs = handle
            .get_runtime_state()
            .map_err(|e| to_py_err(format!("{}", e)))?;
        Ok(rs.into())
    }

    /// Get connected peers (sync read from local doc).
    fn get_peers_sync(&self) -> PyResult<Vec<(String, String)>> {
        let st = self.state.blocking_lock();
        let handle = st
            .handle
            .as_ref()
            .ok_or_else(|| to_py_err("Not connected"))?;
        Ok(handle.get_peers())
    }

    // =========================================================================
    // Repr, context manager, close
    // =========================================================================

    /// Close the session, disconnecting from the notebook room.
    ///
    /// Drops the document handle so the daemon sees this peer as disconnected.
    /// Does NOT shut down the kernel — the daemon manages kernel lifecycle
    /// based on peer count and keep-alive settings.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let mut st = state.lock().await;
            st.handle = None;
            st.broadcast_rx = None;
            Ok(())
        })
    }

    /// Safety net: drop the connection when the Python object is garbage-collected.
    /// Uses `try_lock` to avoid blocking the GC thread.
    fn __del__(&self) {
        if let Ok(mut st) = self.state.try_lock() {
            st.handle = None;
            st.broadcast_rx = None;
        }
    }

    fn __repr__(&self) -> String {
        format!("AsyncSession(id={})", self.notebook_id)
    }

    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move { Ok(slf) })
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let mut st = state.lock().await;
            st.handle = None;
            st.broadcast_rx = None;
            Ok(false)
        })
    }
}
