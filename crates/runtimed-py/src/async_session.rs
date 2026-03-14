//! Async Session for code execution.
//!
//! Thin wrapper around `session_core` async functions, using
//! `future_into_py()` to provide an async Python API.
//! All business logic lives in `session_core.rs`.

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::daemon_paths::get_socket_path;
use crate::error::to_py_err;
use crate::event_stream::ExecutionEventStream;
use crate::session_core::{self, SessionState};
use crate::subscription::EventSubscription;

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
    peer_label: Option<String>,
}

#[pymethods]
impl AsyncSession {
    /// Create a new async session.
    ///
    /// Args:
    ///     notebook_id: Optional unique identifier. If not provided, a random UUID is generated.
    ///     peer_label: Optional label for collaborative presence.
    #[new]
    #[pyo3(signature = (notebook_id=None, peer_label=None))]
    fn new(notebook_id: Option<String>, peer_label: Option<String>) -> PyResult<Self> {
        let notebook_id =
            notebook_id.unwrap_or_else(|| format!("agent-session-{}", uuid::Uuid::new_v4()));

        Ok(Self {
            state: Arc::new(Mutex::new(SessionState::new())),
            notebook_id,
            peer_label,
        })
    }

    /// The notebook ID for this session.
    #[getter]
    fn notebook_id(&self) -> &str {
        &self.notebook_id
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
    // Connection (static constructors and connect)
    // =========================================================================

    /// Open an existing notebook file.
    ///
    /// Returns a coroutine that resolves to a new AsyncSession.
    ///
    /// Args:
    ///     path: Path to the .ipynb file.
    #[staticmethod]
    #[pyo3(signature = (path, peer_label=None))]
    fn open_notebook<'py>(
        py: Python<'py>,
        path: &str,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let path = path.to_string();

        future_into_py(py, async move {
            let socket_path = get_socket_path();
            let (notebook_id, state, _info) =
                session_core::connect_open(socket_path, &path).await?;

            Ok(AsyncSession {
                state: Arc::new(Mutex::new(state)),
                notebook_id,
                peer_label,
            })
        })
    }

    /// Create a new notebook.
    ///
    /// Returns a coroutine that resolves to a new AsyncSession.
    ///
    /// Args:
    ///     runtime: Kernel runtime type (default: "python").
    ///     working_dir: Optional working directory for the notebook.
    #[staticmethod]
    #[pyo3(signature = (runtime="python", working_dir=None, peer_label=None))]
    fn create_notebook<'py>(
        py: Python<'py>,
        runtime: &str,
        working_dir: Option<&str>,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Validate working_dir if provided
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

        let runtime = runtime.to_string();
        let working_dir_buf = working_dir.map(PathBuf::from);

        future_into_py(py, async move {
            let socket_path = get_socket_path();
            let (notebook_id, state, _info) =
                session_core::connect_create(socket_path, &runtime, working_dir_buf).await?;

            Ok(AsyncSession {
                state: Arc::new(Mutex::new(state)),
                notebook_id,
                peer_label,
            })
        })
    }

    /// Connect to the daemon.
    fn connect<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        future_into_py(py, async move {
            session_core::connect(&state, &notebook_id).await
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
        let notebook_id = self.notebook_id.clone();
        let path = path.map(|s| s.to_string());

        future_into_py(py, async move {
            // Ensure connected
            session_core::connect(&state, &notebook_id).await?;
            session_core::save(&state, path.as_deref()).await
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

    /// Create a cell, execute it, and return the result.
    ///
    /// Args:
    ///     code: The code to execute.
    ///     timeout_secs: Maximum time to wait (default: 60).
    ///
    /// Returns a coroutine that resolves to ExecutionResult.
    #[pyo3(signature = (code, timeout_secs=60.0))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        code: &str,
        timeout_secs: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let code = code.to_string();

        future_into_py(py, async move {
            session_core::run(&state, &notebook_id, &code, timeout_secs).await
        })
    }

    /// Queue a cell for execution without waiting for the result.
    fn queue_cell<'py>(&self, py: Python<'py>, cell_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let cell_id = cell_id.to_string();
        future_into_py(py, async move {
            session_core::queue_cell(&state, &cell_id).await
        })
    }

    /// Stream execution events for a cell as an async iterator.
    ///
    /// Unlike execute_cell() which blocks until completion, this returns
    /// an async iterator that yields ExecutionEvent objects as they arrive.
    ///
    /// Args:
    ///     cell_id: The cell ID to execute.
    ///     timeout_secs: Maximum time per event (default: 60).
    ///     signal_only: If True, output events contain only index, not data.
    ///
    /// Returns a coroutine that resolves to ExecutionEventStream.
    #[pyo3(signature = (cell_id, timeout_secs=60.0, signal_only=false))]
    fn stream_execute<'py>(
        &self,
        py: Python<'py>,
        cell_id: &str,
        timeout_secs: f64,
        signal_only: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let notebook_id = self.notebook_id.clone();
        let cell_id = cell_id.to_string();

        future_into_py(py, async move {
            let (broadcast_rx, blob_base_url, blob_store_path) =
                session_core::prepare_stream_execute(&state, &notebook_id, &cell_id).await?;

            Ok(ExecutionEventStream::new(
                broadcast_rx,
                cell_id,
                timeout_secs,
                blob_base_url,
                blob_store_path,
                signal_only,
            ))
        })
    }

    /// Subscribe to execution events for specific cells or event types.
    ///
    /// Returns an async iterator subscription that yields events as they arrive.
    ///
    /// Args:
    ///     cell_ids: Optional list of cell IDs to filter (None = all cells).
    ///     event_types: Optional list of event types to filter (None = all types).
    #[pyo3(signature = (cell_ids=None, event_types=None))]
    fn subscribe(
        &self,
        cell_ids: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
    ) -> PyResult<EventSubscription> {
        // Use a temporary runtime since this returns a sync object (the subscription)
        // not a coroutine.
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| to_py_err(format!("Failed to create runtime: {}", e)))?;

        let (broadcast_rx, blob_base_url, blob_store_path) =
            rt.block_on(session_core::prepare_subscribe(&self.state))?;

        Ok(EventSubscription::new(
            broadcast_rx,
            cell_ids,
            event_types,
            blob_base_url,
            blob_store_path,
        ))
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
    // Repr, context manager, close
    // =========================================================================

    /// Close the session (no-op — daemon manages lifecycle).
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move { Ok(()) })
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
        future_into_py(py, async move { Ok(false) })
    }
}
