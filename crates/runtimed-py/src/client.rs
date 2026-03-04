//! DaemonClient for pool operations.
//!
//! Provides access to daemon status, pool information, and room listing.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::runtime::Runtime;

use crate::error::to_py_err;

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
    /// Connects to the daemon at the default socket path, which is
    /// automatically determined based on environment variables
    /// (RUNTIMED_WORKSPACE_PATH for dev mode).
    #[new]
    fn new() -> PyResult<Self> {
        let runtime = Runtime::new().map_err(to_py_err)?;
        let client = runtimed::client::PoolClient::default();
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
