//! DaemonClient for pool operations.
//!
//! Provides access to daemon status, pool information, and room listing.

use std::path::PathBuf;

use crate::daemon_paths::get_socket_path;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::runtime::Runtime;

use crate::error::{emit_deprecation_warning, to_py_err};
use crate::session::Session;

/// Client for communicating with the runtimed daemon.
///
/// Provides synchronous access to daemon operations. Uses an internal
/// tokio runtime to execute async operations.
///
/// Example:
///     client = DaemonClient()
///     if client.ping():
///         status = client.status()
///         print(f"UV available: {status['uv_available']}")
#[pyclass]
pub struct DaemonClient {
    runtime: Runtime,
    client: runtimed::client::PoolClient,
}

#[pymethods]
impl DaemonClient {
    /// Create a new daemon client.
    ///
    /// Connects to the daemon socket. Respects RUNTIMED_SOCKET_PATH env var
    /// if set, otherwise falls back to the default path (which uses
    /// RUNTIMED_WORKSPACE_PATH for dev mode).
    #[new]
    fn new(py: Python<'_>) -> PyResult<Self> {
        emit_deprecation_warning(
            py,
            "DaemonClient() is deprecated. Use Client() or AsyncClient() instead.",
        )?;
        let runtime = Runtime::new().map_err(to_py_err)?;
        let socket_path = get_socket_path();
        let client = runtimed::client::PoolClient::new(socket_path);
        Ok(Self { runtime, client })
    }

    /// Ping the daemon to check if it's alive.
    ///
    /// Returns True if the daemon responded, False otherwise.
    fn ping(&self) -> bool {
        self.runtime.block_on(self.client.ping()).is_ok()
    }

    /// Check if the daemon is running.
    fn is_running(&self) -> bool {
        self.runtime.block_on(self.client.is_daemon_running())
    }

    /// Get pool statistics.
    ///
    /// Returns a dictionary with pool status:
    ///   - uv_available: number of prewarmed UV environments
    ///   - conda_available: number of prewarmed Conda environments
    ///   - uv_warming: number of UV environments being created
    ///   - conda_warming: number of Conda environments being created
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

    /// List all active notebook rooms.
    ///
    /// Returns a list of dictionaries with room information:
    ///   - notebook_id: the notebook's identifier (file path or virtual ID)
    ///   - active_peers: number of connected peers
    ///   - has_kernel: whether a kernel is running
    ///   - kernel_type: kernel type if running (e.g., "python", "deno")
    ///   - kernel_status: current kernel status (if any)
    fn list_rooms<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
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
    ///
    /// This clears the prewarmed environment pool and triggers
    /// creation of new environments with current settings.
    fn flush_pool(&self) -> PyResult<()> {
        self.runtime
            .block_on(self.client.flush_pool())
            .map_err(to_py_err)
    }

    /// Request daemon shutdown.
    ///
    /// Note: In development mode, this will stop the worktree daemon.
    /// In production, this will stop the system daemon service.
    fn shutdown(&self) -> PyResult<()> {
        self.runtime
            .block_on(self.client.shutdown())
            .map_err(to_py_err)
    }

    fn __repr__(&self) -> String {
        let status = if self.ping() {
            "connected"
        } else {
            "disconnected"
        };
        format!("DaemonClient({})", status)
    }
}

// =========================================================================
// New Client API
// =========================================================================

/// Synchronous client for the runtimed daemon.
///
/// Primary entry point for the runtimed Python API. Creates pre-connected
/// sessions for notebook operations and provides daemon-level operations.
///
/// Example:
///     client = Client()
///     session = client.open_notebook("/path/to/notebook.ipynb")
///     cell_ids = session.get_cell_ids()
#[pyclass]
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

    /// List all active notebook rooms.
    fn list_rooms<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
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

    /// Request daemon shutdown.
    fn shutdown(&self) -> PyResult<()> {
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
    /// Args:
    ///     notebook_id: The notebook room ID to join.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (notebook_id, peer_label=None))]
    fn join_notebook(&self, notebook_id: &str, peer_label: Option<String>) -> PyResult<Session> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        Session::join_notebook_with_socket(self.socket_path.clone(), notebook_id, label)
    }

    fn __repr__(&self) -> String {
        let status = if self.ping() {
            "connected"
        } else {
            "disconnected"
        };
        format!("Client({})", status)
    }
}
