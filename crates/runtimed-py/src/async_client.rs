//! AsyncClient for async daemon operations and session creation.
//!
//! Async counterpart to `Client`. Uses `future_into_py` for all operations.

use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;

use crate::async_session::AsyncSession;
use crate::daemon_paths::get_socket_path;
use crate::error::to_py_err;

/// Room info data for async serialization (avoids needing GIL inside future).
struct RoomInfoData {
    notebook_id: String,
    active_peers: usize,
    has_kernel: bool,
    kernel_type: Option<String>,
    kernel_status: Option<String>,
    env_source: Option<String>,
}

impl<'py> pyo3::IntoPyObject<'py> for RoomInfoData {
    type Target = pyo3::types::PyDict;
    type Output = pyo3::Bound<'py, pyo3::types::PyDict>;
    type Error = PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("notebook_id", &self.notebook_id)?;
        dict.set_item("active_peers", self.active_peers)?;
        dict.set_item("has_kernel", self.has_kernel)?;
        if let Some(kernel_type) = &self.kernel_type {
            dict.set_item("kernel_type", kernel_type)?;
        }
        if let Some(kernel_status) = &self.kernel_status {
            dict.set_item("kernel_status", kernel_status)?;
        }
        if let Some(env_source) = &self.env_source {
            dict.set_item("env_source", env_source)?;
        }
        Ok(dict)
    }
}

/// Async client for the runtimed daemon.
///
/// Primary entry point for the async runtimed Python API. Creates pre-connected
/// async sessions for notebook operations and provides daemon-level operations.
///
/// Example:
///     client = AsyncClient()
///     session = await client.open_notebook("/path/to/notebook.ipynb")
///     cell_ids = await session.get_cell_ids()
#[pyclass(name = "NativeAsyncClient")]
pub struct AsyncClient {
    socket_path: PathBuf,
    peer_label: Option<String>,
}

#[pymethods]
impl AsyncClient {
    /// Create a new async client.
    ///
    /// Args:
    ///     socket_path: Optional path to the daemon socket. If not provided,
    ///         uses RUNTIMED_SOCKET_PATH env var or the default path.
    ///     peer_label: Optional label for collaborative presence (e.g., "Claude").
    ///         Applied to all sessions created by this client unless overridden.
    #[new]
    #[pyo3(signature = (socket_path=None, peer_label=None))]
    fn new(socket_path: Option<String>, peer_label: Option<String>) -> Self {
        let socket_path = socket_path
            .map(PathBuf::from)
            .unwrap_or_else(get_socket_path);
        Self {
            socket_path,
            peer_label,
        }
    }

    /// Ping the daemon to check if it's alive.
    fn ping<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            Ok(client.ping().await.is_ok())
        })
    }

    /// Check if the daemon is running.
    fn is_running<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            Ok(client.is_daemon_running().await)
        })
    }

    /// Get pool statistics.
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            let stats = client.status().await.map_err(to_py_err)?;
            let mut map = std::collections::HashMap::new();
            map.insert("uv_available".to_string(), stats.uv_available as i64);
            map.insert("conda_available".to_string(), stats.conda_available as i64);
            map.insert("uv_warming".to_string(), stats.uv_warming as i64);
            map.insert("conda_warming".to_string(), stats.conda_warming as i64);
            Ok(map)
        })
    }

    /// List all active notebooks.
    ///
    /// Returns a list of dicts with notebook information:
    ///   - notebook_id: the notebook's identifier (file path or virtual ID)
    ///   - active_peers: number of connected peers (int)
    ///   - has_kernel: whether a kernel is running (bool)
    ///   - kernel_type: kernel type if running (e.g., "python", "deno")
    ///   - kernel_status: current kernel status (if any)
    ///   - env_source: environment source label (if any)
    fn list_active_notebooks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            let rooms = client.list_rooms().await.map_err(to_py_err)?;
            // Return as Vec of RoomInfoData which can be converted to Python dicts
            let result: Vec<RoomInfoData> = rooms
                .into_iter()
                .map(|room| RoomInfoData {
                    notebook_id: room.notebook_id,
                    active_peers: room.active_peers,
                    has_kernel: room.has_kernel,
                    kernel_type: room.kernel_type,
                    kernel_status: room.kernel_status,
                    env_source: room.env_source,
                })
                .collect();
            Ok(result)
        })
    }

    /// Flush all pooled environments and rebuild.
    fn flush_pool<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            client.flush_pool().await.map_err(to_py_err)
        })
    }

    /// Close the client connection.
    ///
    /// Releases local resources without affecting the daemon. The native
    /// client is stateless (new connection per RPC), so this is a no-op
    /// today — but having it lets Python callers use `async with` and
    /// gives us room to add connection pooling later.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move { Ok(()) })
    }

    /// Request the daemon process to shut down.
    ///
    /// This stops the *entire* daemon, disconnecting all peers and notebooks.
    /// Callers almost certainly want ``close()`` instead.
    #[pyo3(name = "_shutdown_daemon")]
    fn shutdown_daemon<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let socket_path = self.socket_path.clone();
        future_into_py(py, async move {
            let client = runtimed::client::PoolClient::new(socket_path);
            client.shutdown().await.map_err(to_py_err)
        })
    }

    // =========================================================================
    // Session factory methods
    // =========================================================================

    /// Open an existing notebook file and return a connected AsyncSession.
    ///
    /// Args:
    ///     path: Path to the .ipynb file.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (path, peer_label=None))]
    fn open_notebook<'py>(
        &self,
        py: Python<'py>,
        path: &str,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        let socket_path = self.socket_path.clone();
        let path = path.to_string();
        future_into_py(py, async move {
            AsyncSession::open_notebook_async(socket_path, path, label).await
        })
    }

    /// Create a new notebook and return a connected AsyncSession.
    ///
    /// Args:
    ///     runtime: Kernel runtime type (default: "python").
    ///     working_dir: Optional working directory for environment detection.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (runtime="python", working_dir=None, peer_label=None))]
    fn create_notebook<'py>(
        &self,
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

        let label = peer_label.or_else(|| self.peer_label.clone());
        let socket_path = self.socket_path.clone();
        let runtime = runtime.to_string();
        let working_dir_buf = working_dir.map(PathBuf::from);
        future_into_py(py, async move {
            AsyncSession::create_notebook_async(socket_path, runtime, working_dir_buf, label).await
        })
    }

    /// Join an existing notebook room by ID and return a connected AsyncSession.
    ///
    /// Args:
    ///     notebook_id: The notebook room ID to join.
    ///     peer_label: Optional label override (defaults to client's peer_label).
    #[pyo3(signature = (notebook_id, peer_label=None))]
    fn join_notebook<'py>(
        &self,
        py: Python<'py>,
        notebook_id: &str,
        peer_label: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let label = peer_label.or_else(|| self.peer_label.clone());
        let socket_path = self.socket_path.clone();
        let notebook_id = notebook_id.to_string();
        future_into_py(py, async move {
            AsyncSession::join_notebook_async(socket_path, notebook_id, label).await
        })
    }

    fn __repr__(&self) -> String {
        format!("NativeAsyncClient(socket={})", self.socket_path.display())
    }
}
