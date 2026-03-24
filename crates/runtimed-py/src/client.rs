//! NativeClient for synchronous daemon operations.
//!
//! Provides synchronous access to daemon status, pool information, and session creation.

use std::path::PathBuf;

use crate::daemon_paths::get_socket_path;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::runtime::Runtime;

use crate::daemon_paths::resolve_notebook_path;
use crate::error::to_py_err;
use crate::session::Session;

/// Synchronous native client for the runtimed daemon.
///
/// Low-level client — the Python `runtimed.Client` wraps this to return
/// `Notebook` objects instead of raw `Session`.
///
/// Example:
///     client = NativeClient()
///     session = client.open_notebook("/path/to/notebook.ipynb")
///     cell_ids = session.get_cell_ids()
#[pyclass(name = "NativeClient")]
pub struct Client {
    runtime: Runtime,
    client: runtimed::client::PoolClient,
    socket_path: PathBuf,
    peer_label: Option<String>,
}

#[pymethods]
impl Client {
    /// Create a new client.
    ///
    /// Args:
    ///     socket_path: Optional path to the daemon socket. If not provided,
    ///         uses RUNTIMED_SOCKET_PATH env var or the default path.
    ///     peer_label: Optional label for collaborative presence (e.g., "Claude").
    ///         Applied to all sessions created by this client unless overridden.
    #[new]
    #[pyo3(signature = (socket_path=None, peer_label=None))]
    fn new(socket_path: Option<String>, peer_label: Option<String>) -> PyResult<Self> {
        let runtime = Runtime::new().map_err(to_py_err)?;
        let socket_path = socket_path
            .map(PathBuf::from)
            .unwrap_or_else(get_socket_path);
        let client = runtimed::client::PoolClient::new(socket_path.clone());
        Ok(Self {
            runtime,
            client,
            socket_path,
            peer_label,
        })
    }

    /// Ping the daemon to check if it's alive.
    fn ping(&self) -> bool {
        self.runtime.block_on(self.client.ping()).is_ok()
    }

    /// Check if the daemon is running.
    fn is_running(&self) -> bool {
        self.runtime.block_on(self.client.is_daemon_running())
    }

    /// Get pool statistics.
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stats = self
            .runtime
            .block_on(self.client.status())
            .map_err(to_py_err)?;

        let dict = PyDict::new(py);
        dict.set_item("uv_available", stats.uv_available)?;
        dict.set_item("conda_available", stats.conda_available)?;
        dict.set_item("uv_warming", stats.uv_warming)?;
        dict.set_item("conda_warming", stats.conda_warming)?;
        Ok(dict)
    }

    /// List all active notebooks.
    fn list_active_notebooks<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rooms = self
            .runtime
            .block_on(self.client.list_rooms())
            .map_err(to_py_err)?;

        let mut result = Vec::with_capacity(rooms.len());
        for room in rooms {
            let dict = PyDict::new(py);
            dict.set_item("notebook_id", &room.notebook_id)?;
            dict.set_item("active_peers", room.active_peers)?;
            dict.set_item("has_kernel", room.has_kernel)?;
            if let Some(kernel_type) = &room.kernel_type {
                dict.set_item("kernel_type", kernel_type)?;
            }
            if let Some(kernel_status) = &room.kernel_status {
                dict.set_item("kernel_status", kernel_status)?;
            }
            if let Some(env_source) = &room.env_source {
                dict.set_item("env_source", env_source)?;
            }
            result.push(dict);
        }
        Ok(result)
    }

    /// Flush all pooled environments and rebuild.
    fn flush_pool(&self) -> PyResult<()> {
        self.runtime
            .block_on(self.client.flush_pool())
            .map_err(to_py_err)
    }

    /// Close the client connection.
    ///
    /// Releases local resources without affecting the daemon. No-op today
    /// (the native client is stateless), but gives callers a clear,
    /// non-destructive cleanup path.
    fn close(&self) -> PyResult<()> {
        Ok(())
    }

    /// Request the daemon process to shut down.
    ///
    /// This stops the *entire* daemon, disconnecting all peers and notebooks.
    /// Callers almost certainly want ``close()`` instead.
    #[pyo3(name = "_shutdown_daemon")]
    fn shutdown_daemon(&self) -> PyResult<()> {
        self.runtime
            .block_on(self.client.shutdown())
            .map_err(to_py_err)
    }

    // =========================================================================
    // Session factory methods
    // =========================================================================

    /// Open an existing notebook file and return a connected Session.
    ///
    /// Args:
    ///     path: Path to the .ipynb file.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (path, peer_label=None))]
    fn open_notebook(&self, path: &str, peer_label: Option<String>) -> PyResult<Session> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        Session::open_notebook_with_socket(self.socket_path.clone(), path, label)
    }

    /// Create a new notebook and return a connected Session.
    ///
    /// Args:
    ///     runtime: Kernel runtime type (default: "python").
    ///     working_dir: Optional working directory for environment detection.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (runtime="python", working_dir=None, peer_label=None))]
    fn create_notebook(
        &self,
        runtime: &str,
        working_dir: Option<&str>,
        peer_label: Option<String>,
    ) -> PyResult<Session> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        Session::create_notebook_with_socket(self.socket_path.clone(), runtime, working_dir, label)
    }

    /// Join an existing notebook room by ID and return a connected Session.
    ///
    /// Relative paths (e.g. ``"notebook.ipynb"``) are resolved to absolute
    /// paths so they match the canonical room keys used by the daemon.
    ///
    /// Args:
    ///     notebook_id: The notebook room ID to join (UUID or file path).
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (notebook_id, peer_label=None))]
    fn join_notebook(&self, notebook_id: &str, peer_label: Option<String>) -> PyResult<Session> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        let resolved = resolve_notebook_path(notebook_id);
        Session::join_notebook_with_socket(self.socket_path.clone(), &resolved, label)
    }

    fn __repr__(&self) -> String {
        let status = if self.ping() {
            "connected"
        } else {
            "disconnected"
        };
        format!("NativeClient({})", status)
    }
}
