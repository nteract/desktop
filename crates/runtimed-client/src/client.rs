//! Client for communicating with the pool daemon.
//!
//! Notebook windows use this client to request prewarmed environments
//! from the central daemon via IPC (Unix domain sockets on Unix, named pipes
//! on Windows).

use std::path::PathBuf;
use std::time::Duration;

use log::{info, warn};
use tokio::io::{AsyncRead, AsyncWrite};

use serde::Serialize;

use crate::connection::{self, Handshake};
use crate::protocol::{Request, Response};
use crate::{default_socket_path, EnvType, PoolState, PooledEnv};

/// Progress updates during daemon startup.
///
/// Emitted by `ensure_daemon_running` to allow UI feedback during
/// first-launch installation or daemon restart.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonProgress {
    /// Checking if daemon is already running
    Checking,
    /// Installing daemon service (first launch)
    Installing,
    /// Upgrading daemon to new version
    Upgrading,
    /// Starting daemon service
    Starting,
    /// Waiting for daemon to become ready
    WaitingForReady { attempt: u32, max_attempts: u32 },
    /// Daemon is ready
    Ready { endpoint: String },
    /// Daemon failed to start
    Failed {
        error: String,
        /// Actionable guidance for the user
        guidance: String,
    },
}

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

/// Result of inspecting a notebook's state.
#[derive(Debug, Clone)]
pub struct InspectResult {
    pub notebook_id: String,
    pub cells: Vec<crate::notebook_doc::CellSnapshot>,
    /// Outputs keyed by cell_id. Outputs live in `RuntimeStateDoc` keyed
    /// by `execution_id` and travel alongside the cell snapshot rather
    /// than on it. Cells without outputs are absent from the map.
    pub outputs_by_cell: std::collections::HashMap<String, Vec<serde_json::Value>>,
    pub source: String,
    pub kernel_info: Option<crate::protocol::NotebookKernelInfo>,
}

/// Version information returned by a daemon ping.
#[derive(Debug, Clone)]
pub struct PongInfo {
    /// Numeric protocol version (matches `PROTOCOL_VERSION` in connection.rs).
    /// `None` if the daemon is too old to include this field.
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    /// `None` if the daemon is too old to include this field.
    pub daemon_version: Option<String>,
}

/// Rich daemon metadata. Replaces the `daemon.json` sidecar file —
/// query this from the daemon directly via `PoolClient::daemon_info()`.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    /// Numeric protocol version.
    pub protocol_version: u32,
    /// Daemon version string (e.g., "2.0.0+abc123").
    pub daemon_version: String,
    /// Daemon process ID.
    pub pid: u32,
    /// When the daemon process began.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// HTTP port for the content-addressed blob server. `None` if the
    /// blob server hasn't finished binding yet.
    pub blob_port: Option<u16>,
    /// Path to the git worktree this dev daemon is pinned to.
    pub worktree_path: Option<String>,
    /// Human-readable workspace description (dev mode only).
    pub workspace_description: Option<String>,
}

impl PongInfo {
    /// Check whether the daemon's protocol version is compatible with this client.
    ///
    /// Returns `Ok(())` if compatible, or an `Err` with an actionable message
    /// explaining the mismatch. If the daemon didn't report a protocol version
    /// (old daemon), this logs a warning but does not error — backward
    /// compatibility is preserved.
    pub fn check_protocol_version(&self) -> Result<(), String> {
        let expected = notebook_protocol::connection::PROTOCOL_VERSION;

        match self.protocol_version {
            Some(remote) if remote == expected => Ok(()),
            Some(remote) if remote > expected => Err(format!(
                "Daemon is running protocol version {remote}, but this CLI expects version {expected}. \
                 Please update the CLI (or reinstall the app) to match the daemon."
            )),
            Some(remote) => Err(format!(
                "Daemon is running protocol version {remote}, but this CLI expects version {expected}. \
                 Please update the daemon: runt daemon doctor --fix"
            )),
            None => {
                log::warn!(
                    "[pool-client] Daemon did not report a protocol version — \
                     it may be outdated. Consider updating: runt daemon doctor --fix"
                );
                Ok(())
            }
        }
    }
}

/// Error type for client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Failed to connect to daemon: {0}")]
    ConnectionFailed(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Daemon returned error: {0}")]
    DaemonError(String),

    #[error("Connection timeout")]
    Timeout,
}

/// Client for the pool daemon.
pub struct PoolClient {
    socket_path: PathBuf,
    connect_timeout: Duration,
}

impl Default for PoolClient {
    fn default() -> Self {
        Self::new(default_socket_path())
    }
}

impl PoolClient {
    /// Create a new client with a custom socket/pipe path.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            connect_timeout: Duration::from_secs(2),
        }
    }

    /// Set the connection timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Check if the daemon is running.
    pub async fn is_daemon_running(&self) -> bool {
        self.ping().await.is_ok()
    }

    /// Ping the daemon to check if it's alive.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.ping_version().await.map(|_| ())
    }

    /// Ping the daemon and return its version info.
    ///
    /// Returns `Ok(PongInfo)` with the daemon's protocol version and
    /// version string. Old daemons that send a bare `Pong` (no fields)
    /// will have `None` for both fields.
    pub async fn ping_version(&self) -> Result<PongInfo, ClientError> {
        let response = self.send_request(Request::Ping).await?;
        match response {
            Response::Pong {
                protocol_version,
                daemon_version,
            } => Ok(PongInfo {
                protocol_version,
                daemon_version,
            }),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Query rich daemon metadata (pid, blob port, worktree info, etc.).
    ///
    /// This is the socket-based replacement for reading `daemon.json`.
    /// Returns `Err` with `DaemonError("Unknown request")` on daemons too
    /// old to know this request — callers can either treat that as
    /// "metadata unavailable" or fall back to the file (we don't bake in
    /// a file fallback here because the whole point is to stop relying
    /// on the file).
    pub async fn daemon_info(&self) -> Result<DaemonInfo, ClientError> {
        let response = self.send_request(Request::GetDaemonInfo).await?;
        match response {
            Response::DaemonInfo {
                protocol_version,
                daemon_version,
                pid,
                started_at,
                blob_port,
                worktree_path,
                workspace_description,
            } => Ok(DaemonInfo {
                protocol_version,
                daemon_version,
                pid,
                started_at,
                blob_port,
                worktree_path,
                workspace_description,
            }),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Request an environment from the pool.
    ///
    /// Returns `Ok(Some(env))` if an environment was available,
    /// `Ok(None)` if the pool was empty.
    pub async fn take(&self, env_type: EnvType) -> Result<Option<PooledEnv>, ClientError> {
        let response = self.send_request(Request::Take { env_type }).await?;
        match response {
            Response::Env { env } => {
                info!(
                    "[pool-client] Got {} env from daemon: {:?}",
                    env_type, env.venv_path
                );
                Ok(Some(env))
            }
            Response::Empty => {
                info!("[pool-client] Daemon pool empty for {}", env_type);
                Ok(None)
            }
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Return an environment to the pool.
    pub async fn return_env(&self, env: PooledEnv) -> Result<(), ClientError> {
        let response = self.send_request(Request::Return { env }).await?;
        match response {
            Response::Returned => Ok(()),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Get pool state (from PoolDoc).
    pub async fn status(&self) -> Result<PoolState, ClientError> {
        let response = self.send_request(Request::Status).await?;
        match response {
            Response::Stats { state } => Ok(state),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Get environment paths currently in use by running kernels.
    pub async fn active_env_paths(&self) -> Result<Vec<std::path::PathBuf>, ClientError> {
        let response = self.send_request(Request::ActiveEnvPaths).await?;
        match response {
            Response::ActiveEnvPaths { paths } => Ok(paths),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Flush all pooled environments and trigger rebuild with current settings.
    pub async fn flush_pool(&self) -> Result<(), ClientError> {
        let response = self.send_request(Request::FlushPool).await?;
        match response {
            Response::Flushed => Ok(()),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Request daemon shutdown.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let response = self.send_request(Request::Shutdown).await?;
        match response {
            Response::ShuttingDown => Ok(()),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Inspect a notebook's Automerge state.
    pub async fn inspect_notebook(&self, notebook_id: &str) -> Result<InspectResult, ClientError> {
        let response = self
            .send_request(Request::InspectNotebook {
                notebook_id: notebook_id.to_string(),
            })
            .await?;
        match response {
            Response::NotebookState {
                notebook_id,
                cells,
                outputs_by_cell,
                source,
                kernel_info,
            } => Ok(InspectResult {
                notebook_id,
                cells,
                outputs_by_cell,
                source,
                kernel_info,
            }),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// List all active notebook rooms.
    pub async fn list_rooms(&self) -> Result<Vec<crate::protocol::RoomInfo>, ClientError> {
        let response = self.send_request(Request::ListRooms).await?;
        match response {
            Response::RoomsList { rooms } => Ok(rooms),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Shutdown a notebook's kernel and evict its room.
    ///
    /// Returns `Ok(true)` if the notebook was found and shut down,
    /// `Ok(false)` if no such notebook was open.
    pub async fn shutdown_notebook(&self, notebook_id: &str) -> Result<bool, ClientError> {
        let response = self
            .send_request(Request::ShutdownNotebook {
                notebook_id: notebook_id.to_string(),
            })
            .await?;
        match response {
            Response::NotebookShutdown { found } => Ok(found),
            Response::Error { message } => Err(ClientError::DaemonError(message)),
            _ => Err(ClientError::ProtocolError(
                "Unexpected response".to_string(),
            )),
        }
    }

    /// Send a request to the daemon and receive a response.
    ///
    /// The entire request (connect + send + recv) is bounded by a timeout
    /// derived from `connect_timeout` so that a bound-but-not-yet-accepting
    /// socket cannot stall the caller indefinitely.
    async fn send_request(&self, request: Request) -> Result<Response, ClientError> {
        // Overall timeout: connect_timeout + 3s for the request/response exchange.
        let overall_timeout = self.connect_timeout + Duration::from_secs(3);
        tokio::time::timeout(overall_timeout, self.send_request_inner(request))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn send_request_inner(&self, request: Request) -> Result<Response, ClientError> {
        #[cfg(unix)]
        let stream = {
            let connect_result =
                tokio::time::timeout(self.connect_timeout, UnixStream::connect(&self.socket_path))
                    .await;

            match connect_result {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err(ClientError::ConnectionFailed(e)),
                Err(_) => return Err(ClientError::Timeout),
            }
        };

        #[cfg(windows)]
        let stream = {
            let pipe_name = self.socket_path.to_string_lossy().to_string();
            let connect_result = tokio::time::timeout(self.connect_timeout, async {
                // Named pipes may need retry if server is between connections
                let mut attempts = 0;
                loop {
                    match ClientOptions::new().open(&pipe_name) {
                        Ok(client) => return Ok(client),
                        Err(_) if attempts < 5 => {
                            attempts += 1;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(e) => return Err(e),
                    }
                }
            })
            .await;

            match connect_result {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err(ClientError::ConnectionFailed(e)),
                Err(_) => return Err(ClientError::Timeout),
            }
        };

        self.send_request_on_stream(stream, request).await
    }

    /// Send a request on an established stream.
    async fn send_request_on_stream<S>(
        &self,
        mut stream: S,
        request: Request,
    ) -> Result<Response, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // Send preamble (magic bytes + protocol version)
        connection::send_preamble(&mut stream)
            .await
            .map_err(|e| ClientError::ProtocolError(format!("preamble: {}", e)))?;

        // Send the channel handshake
        connection::send_json_frame(&mut stream, &Handshake::Pool)
            .await
            .map_err(|e| ClientError::ProtocolError(format!("handshake: {}", e)))?;

        // Send the request as a framed JSON message
        connection::send_json_frame(&mut stream, &request)
            .await
            .map_err(|e| ClientError::ProtocolError(format!("send: {}", e)))?;

        // Read the response
        connection::recv_json_frame::<_, Response>(&mut stream)
            .await
            .map_err(|e| ClientError::ProtocolError(format!("recv: {}", e)))?
            .ok_or_else(|| ClientError::ProtocolError("connection closed".to_string()))
    }
}

/// Try to get an environment from the daemon, falling back gracefully.
///
/// This is a convenience function that:
/// 1. Tries to connect to the daemon
/// 2. If successful, requests an environment
/// 3. If daemon is unavailable or pool is empty, returns None
///
/// This allows notebook code to optionally use the daemon without requiring it.
pub async fn try_get_pooled_env(env_type: EnvType) -> Option<PooledEnv> {
    let client = PoolClient::default();

    match client.take(env_type).await {
        Ok(Some(env)) => Some(env),
        Ok(None) => {
            info!(
                "[pool-client] Daemon pool empty for {}, will create locally",
                env_type
            );
            None
        }
        Err(e) => {
            warn!(
                "[pool-client] Could not connect to daemon ({:?}), will create locally",
                e
            );
            None
        }
    }
}

/// Ensure the pool daemon is running, installing and starting it if needed.
///
/// This function:
/// 1. Checks if the daemon responds to ping
/// 2. If running, checks version - upgrades if bundled version differs
/// 3. If not responding, checks if the service is installed
/// 4. If not installed, installs the service using the provided binary path
/// 5. Starts the service if not running
/// 6. Waits for the daemon to be ready
///
/// In development mode (RUNTIMED_DEV=1 or RUNTIMED_WORKSPACE_PATH set):
/// - Skips service installation/upgrade
/// - Only checks if the per-worktree daemon is running
/// - Returns an error with guidance if not running
///
/// The optional `on_progress` callback receives `DaemonProgress` updates
/// during startup, useful for showing UI feedback.
///
/// Returns Ok(endpoint) if daemon is running, Err if it couldn't be started.
pub async fn ensure_daemon_running<F>(
    daemon_binary: Option<std::path::PathBuf>,
    on_progress: Option<F>,
) -> Result<String, EnsureDaemonError>
where
    F: Fn(DaemonProgress),
{
    use crate::service::ServiceManager;
    use crate::singleton::query_daemon_info;

    // Helper to emit progress if callback is provided
    let emit = |progress: DaemonProgress| {
        if let Some(ref cb) = on_progress {
            cb(progress);
        }
    };

    let client = PoolClient::default();

    emit(DaemonProgress::Checking);

    // In dev mode, skip service management - just check if daemon is running
    if crate::is_dev_mode() {
        info!("[pool-client] Development mode: checking for worktree daemon...");

        if let Ok(pong) = client.ping_version().await {
            if let Err(msg) = pong.check_protocol_version() {
                warn!("[pool-client] {}", msg);
            }
            if let Some(ref ver) = pong.daemon_version {
                info!("[pool-client] Dev daemon version: {}", ver);
            }
            if let Some(info) = query_daemon_info(crate::default_socket_path()).await {
                emit(DaemonProgress::Ready {
                    endpoint: info.endpoint.clone(),
                });
                info!(
                    "[pool-client] Dev daemon running at {} (worktree: {:?})",
                    info.endpoint, info.worktree_path
                );
                return Ok(info.endpoint);
            }
        }

        // Dev daemon not running - provide helpful error
        let socket_path = crate::default_socket_path();
        emit(DaemonProgress::Failed {
            error: "Dev daemon not running".to_string(),
            guidance: "Start it with: cargo xtask dev-daemon".to_string(),
        });
        return Err(EnsureDaemonError::DevDaemonNotRunning(socket_path));
    }

    // Production mode: full service management
    let mut manager = ServiceManager::default();

    // Version of the bundled/calling binary (includes git commit for dev builds)
    let bundled_version = format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        include_str!(concat!(env!("OUT_DIR"), "/git_hash.txt"))
    );

    // First, try to ping the daemon
    if client.ping().await.is_ok() {
        if let Some(info) = query_daemon_info(crate::default_socket_path()).await {
            // Check if we need to upgrade
            if info.version != bundled_version {
                info!(
                    "[pool-client] Version mismatch: running={}, bundled={}",
                    info.version, bundled_version
                );

                if let Some(binary_path) = &daemon_binary {
                    if !binary_path.exists() {
                        emit(DaemonProgress::Failed {
                            error: format!("Binary not found: {}", binary_path.display()),
                            guidance:
                                "The bundled daemon binary is missing. Try reinstalling the app."
                                    .to_string(),
                        });
                        return Err(EnsureDaemonError::BinaryNotFound(binary_path.clone()));
                    }

                    emit(DaemonProgress::Upgrading);
                    info!("[pool-client] Upgrading daemon...");
                    if let Err(e) = manager.upgrade(binary_path) {
                        emit(DaemonProgress::Failed {
                            error: format!("Upgrade failed: {}", e),
                            guidance: "Run 'runt daemon doctor --fix' to diagnose and repair."
                                .to_string(),
                        });
                        return Err(EnsureDaemonError::UpgradeFailed(e.to_string()));
                    }

                    // Wait for upgraded daemon to be ready
                    return wait_for_daemon_ready(&client, &emit).await;
                } else {
                    // No binary path provided, can't upgrade - just use existing
                    info!("[pool-client] No binary path provided, using existing daemon");
                }
            }

            emit(DaemonProgress::Ready {
                endpoint: info.endpoint.clone(),
            });
            info!("[pool-client] Daemon already running at {}", info.endpoint);
            return Ok(info.endpoint);
        }
    }

    info!("[pool-client] Daemon not responding, checking service...");

    // Install if not already installed
    if !manager.is_installed() {
        info!("[pool-client] Service not installed, installing...");

        let binary_path = daemon_binary.ok_or_else(|| {
            emit(DaemonProgress::Failed {
                error: "No binary path provided for installation".to_string(),
                guidance: "Launch the app from the installed location, not directly from the DMG."
                    .to_string(),
            });
            EnsureDaemonError::NoBinaryPath
        })?;

        if !binary_path.exists() {
            emit(DaemonProgress::Failed {
                error: format!("Binary not found: {}", binary_path.display()),
                guidance: "The bundled daemon binary is missing. Try reinstalling the app."
                    .to_string(),
            });
            return Err(EnsureDaemonError::BinaryNotFound(binary_path));
        }

        emit(DaemonProgress::Installing);
        if let Err(e) = manager.install(&binary_path) {
            emit(DaemonProgress::Failed {
                error: format!("Install failed: {}", e),
                guidance: "Run 'runt daemon doctor --fix' to diagnose and repair.".to_string(),
            });
            return Err(EnsureDaemonError::InstallFailed(e.to_string()));
        }
    }

    // Start the service
    emit(DaemonProgress::Starting);
    info!("[pool-client] Starting service...");
    if let Err(e) = manager.start() {
        emit(DaemonProgress::Failed {
            error: format!("Start failed: {}", e),
            guidance: "Run 'runt daemon doctor --fix' to diagnose and repair.".to_string(),
        });
        return Err(EnsureDaemonError::StartFailed(e.to_string()));
    }

    // Wait for daemon to be ready
    wait_for_daemon_ready(&client, &emit).await
}

/// Wait for the daemon to become ready (up to 10 seconds).
async fn wait_for_daemon_ready<F>(
    client: &PoolClient,
    emit: &F,
) -> Result<String, EnsureDaemonError>
where
    F: Fn(DaemonProgress),
{
    use crate::singleton::query_daemon_info;

    const MAX_ATTEMPTS: u32 = 20;

    info!("[pool-client] Waiting for daemon to be ready...");
    for i in 0..MAX_ATTEMPTS {
        emit(DaemonProgress::WaitingForReady {
            attempt: i + 1,
            max_attempts: MAX_ATTEMPTS,
        });

        tokio::time::sleep(Duration::from_millis(500)).await;

        if client.ping().await.is_ok() {
            if let Some(info) = query_daemon_info(crate::default_socket_path()).await {
                emit(DaemonProgress::Ready {
                    endpoint: info.endpoint.clone(),
                });
                info!(
                    "[pool-client] Daemon ready at {} (waited {}ms)",
                    info.endpoint,
                    (i + 1) * 500
                );
                return Ok(info.endpoint);
            }
        }
    }

    emit(DaemonProgress::Failed {
        error: "Daemon did not become ready within timeout".to_string(),
        guidance:
            "Check daemon logs with 'runt daemon logs' or run 'runt daemon doctor' to diagnose."
                .to_string(),
    });
    Err(EnsureDaemonError::Timeout)
}

/// Errors that can occur when ensuring the daemon is running.
#[derive(Debug, thiserror::Error)]
pub enum EnsureDaemonError {
    #[error("No daemon binary path provided for installation")]
    NoBinaryPath,

    #[error("Daemon binary not found at {0}")]
    BinaryNotFound(std::path::PathBuf),

    #[error("Failed to install daemon service: {0}")]
    InstallFailed(String),

    #[error("Failed to start daemon service: {0}")]
    StartFailed(String),

    #[error("Failed to upgrade daemon service: {0}")]
    UpgradeFailed(String),

    #[error("Daemon did not become ready within timeout")]
    Timeout,

    #[error("Dev daemon not running at {0}. Start it with: cargo xtask dev-daemon")]
    DevDaemonNotRunning(std::path::PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_default() {
        let client = PoolClient::default();
        #[cfg(unix)]
        assert!(client
            .socket_path
            .to_string_lossy()
            .contains("runtimed.sock"));
        #[cfg(windows)]
        assert!(client.socket_path.to_string_lossy().contains("runtimed"));
    }

    #[test]
    fn test_client_custom_path() {
        let client = PoolClient::new(PathBuf::from("/tmp/test.sock"));
        assert_eq!(client.socket_path, PathBuf::from("/tmp/test.sock"));
    }
}
