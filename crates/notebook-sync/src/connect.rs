//! Connection handshake and sync-task bootstrap.
//!
//! All connect variants use [`NotebookDoc::bootstrap()`] to create the
//! initial local document.  This seeds the doc with the standard notebook
//! skeleton (`schema_version`, empty `cells` map, `metadata`) so that
//! `Automerge::is_empty()` returns false **before** the first sync
//! message arrives.  Without this, `load_incremental`'s empty-doc
//! fast-path replaces `*self` with a freshly-loaded doc, discarding any
//! encoding or actor settings.
//!
//! Establishes a connection to the runtimed daemon, performs the protocol
//! handshake, bootstraps empty local docs, and then hands post-handshake
//! socket ownership to the background sync task.
//!
//! Platform-specific stream creation (Unix socket or Windows named pipe)
//! is handled internally. The handshake and sync logic is generic over
//! `AsyncRead + AsyncWrite`.

use automerge::sync;
use automerge::AutoCommit;
use log::{debug, info};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use notebook_protocol::connection::{
    self, Handshake, NotebookConnectionInfo, ProtocolCapabilities, PROTOCOL_V4,
};
use notebook_protocol::protocol::NotebookBroadcast;

use crate::error::SyncError;
use crate::handle::DocHandle;
use crate::relay::RelayHandle;
use crate::relay_task;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;
use crate::status::SyncStatus;
use crate::sync_task;

/// Result of connecting to a notebook room.
pub struct ConnectResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: crate::BroadcastReceiver,

    /// Initial metadata string (legacy format, for handshake compat).
    pub initial_metadata: Option<String>,
}

/// Result of connecting to an existing notebook file.
pub struct OpenResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: crate::BroadcastReceiver,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,
}

/// Result of creating a new notebook.
pub struct CreateResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: crate::BroadcastReceiver,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,
}

/// Result of opening a notebook as a relay (no local document).
pub struct RelayOpenResult {
    /// Handle for forwarding frames and sending requests.
    pub handle: RelayHandle,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,
}

/// Result of creating a notebook as a relay (no local document).
pub struct RelayCreateResult {
    /// Handle for forwarding frames and sending requests.
    pub handle: RelayHandle,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,
}

/// Platform-specific helper macro to connect to the daemon socket.
///
/// On Unix: `tokio::net::UnixStream::connect`
/// On Windows: `tokio::net::windows::named_pipe::ClientOptions::new().open`
macro_rules! connect_stream {
    ($socket_path:expr) => {{
        let path = $socket_path;
        let result = {
            #[cfg(unix)]
            {
                tokio::net::UnixStream::connect(path).await
            }
            #[cfg(windows)]
            {
                tokio::net::windows::named_pipe::ClientOptions::new().open(path)
            }
        };
        match result {
            Ok(stream) => stream,
            Err(e) => {
                let path_display = path.display();
                return Err(match e.kind() {
                    std::io::ErrorKind::NotFound => SyncError::DaemonUnavailable {
                        message: format!(
                            "Daemon is not running. Endpoint not found at {path_display}."
                        ),
                        source: e,
                    },
                    std::io::ErrorKind::ConnectionRefused => SyncError::DaemonUnavailable {
                        message: format!(
                            "Daemon connection refused at {path_display}. \
                             The daemon may have crashed or is restarting."
                        ),
                        source: e,
                    },
                    std::io::ErrorKind::PermissionDenied => SyncError::DaemonUnavailable {
                        message: format!(
                            "Permission denied connecting to daemon at {path_display}. \
                             Check file permissions."
                        ),
                        source: e,
                    },
                    _ => SyncError::Io(e),
                });
            }
        }
    }};
}

// =========================================================================
// Public connect functions
// =========================================================================

/// Connect to a notebook room by ID.
///
/// Performs the protocol handshake, spawns the background sync task, and
/// returns a `DocHandle` plus a broadcast receiver for kernel events.
///
/// `actor_label` sets the Automerge actor identity **before** initial sync
/// so that even the bootstrap operations are attributed to the caller
/// (e.g., `"agent:claude:abc123"`, `"human:kyle:session42"`).
pub async fn connect(
    socket_path: PathBuf,
    notebook_id: String,
    actor_label: &str,
) -> Result<ConnectResult, SyncError> {
    connect_with_options(socket_path, notebook_id, None, None, actor_label).await
}

/// Connect to a notebook room with options.
pub async fn connect_with_options(
    socket_path: PathBuf,
    notebook_id: String,
    working_dir: Option<PathBuf>,
    initial_metadata: Option<String>,
    actor_label: &str,
) -> Result<ConnectResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send handshake
    let handshake = Handshake::NotebookSync {
        notebook_id: notebook_id.clone(),
        protocol: Some(PROTOCOL_V4.to_string()),
        working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
        initial_metadata: initial_metadata.clone(),
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive protocol capabilities
    let caps_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let caps: ProtocolCapabilities = serde_json::from_slice(&caps_data)?;
    check_daemon_protocol_version(&caps);

    // Start from the standard notebook skeleton so the background sync task
    // owns the entire bootstrap from the first post-handshake frame onward.
    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let doc = bootstrap.into_inner();
    let peer_state = sync::State::new();

    // Build the shared state and channels
    build_and_spawn(doc, peer_state, notebook_id, reader, writer)
        .await
        .map(|(handle, broadcast_rx)| ConnectResult {
            handle,
            broadcast_rx,
            initial_metadata,
        })
}

/// Connect and open an existing notebook file.
pub async fn connect_open(
    socket_path: PathBuf,
    path: PathBuf,
    actor_label: &str,
) -> Result<OpenResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send open handshake
    let handshake = Handshake::OpenNotebook {
        path: path.to_string_lossy().to_string(),
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive connection info
    let info_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let info: NotebookConnectionInfo = serde_json::from_slice(&info_data)?;

    if let Some(ref error) = info.error {
        return Err(SyncError::Protocol(error.clone()));
    }

    let notebook_id = info.notebook_id.clone();

    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let doc = bootstrap.into_inner();
    let peer_state = sync::State::new();

    build_and_spawn(doc, peer_state, notebook_id, reader, writer)
        .await
        .map(|(handle, broadcast_rx)| OpenResult {
            handle,
            broadcast_rx,
            info,
        })
}

/// Connect and create a new notebook.
///
/// The daemon creates an empty notebook room with one code cell and
/// returns connection info with a generated UUID as the notebook_id.
#[allow(clippy::too_many_arguments)]
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<CreateResult, SyncError> {
    connect_create_inner(
        socket_path,
        runtime,
        working_dir,
        None,
        actor_label,
        ephemeral,
        package_manager,
        dependencies,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn connect_create_inner(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<CreateResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send create handshake
    let handshake = Handshake::CreateNotebook {
        runtime: runtime.to_string(),
        working_dir: working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        notebook_id,
        ephemeral: if ephemeral { Some(true) } else { None },
        package_manager,
        dependencies,
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive connection info
    let info_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let info: NotebookConnectionInfo = serde_json::from_slice(&info_data)?;

    if let Some(ref error) = info.error {
        return Err(SyncError::Protocol(error.clone()));
    }

    let notebook_id = info.notebook_id.clone();

    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let doc = bootstrap.into_inner();
    let peer_state = sync::State::new();

    build_and_spawn(doc, peer_state, notebook_id, reader, writer)
        .await
        .map(|(handle, broadcast_rx)| CreateResult {
            handle,
            broadcast_rx,
            info,
        })
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Build the shared state, channels, and spawn the sync task.
///
/// This is the common setup after handshake + initial sync, shared by
/// all connect variants.
async fn build_and_spawn<R, W>(
    doc: AutoCommit,
    peer_state: sync::State,
    notebook_id: String,
    reader: R,
    writer: W,
) -> Result<(DocHandle, crate::BroadcastReceiver), SyncError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut shared_state = SharedDocState::try_new(doc, notebook_id.clone())?;
    shared_state.peer_state = peer_state;

    let shared = Arc::new(Mutex::new(shared_state));

    let initial_snapshot = {
        let state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        NotebookSnapshot::from_doc(&state.doc)
    };

    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (status_tx, status_rx) = watch::channel(SyncStatus::connected_pending());
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<sync_task::SyncCommand>(32);
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(64);

    let handle = DocHandle::new(
        Arc::clone(&shared),
        changed_tx,
        cmd_tx,
        Arc::clone(&snapshot_tx),
        snapshot_rx,
        status_rx,
        notebook_id.clone(),
    );

    let task_config = sync_task::SyncTaskConfig {
        doc: Arc::clone(&shared),
        changed_rx,
        cmd_rx,
        snapshot_tx: Arc::clone(&snapshot_tx),
        status_tx,
        broadcast_tx,
    };

    let notebook_id_for_task = notebook_id;
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Sync task started for {}",
            notebook_id_for_task
        );
        sync_task::run(task_config, reader, writer).await;
        info!(
            "[notebook-sync] Sync task stopped for {}",
            notebook_id_for_task
        );
    });

    Ok((handle, broadcast_rx.into()))
}

// =========================================================================
// Relay connect functions — no initial sync, no local doc
// =========================================================================

/// Open a notebook as a relay — transparent byte pipe, no local document.
///
/// Performs the handshake only (preamble + OpenNotebook + receive info).
/// Does not perform any bootstrap sync on the client side — the daemon's
/// initial sync and session-status frames stay in the socket buffer and get
/// piped to the frontend by the relay task. The frontend (WASM) owns the
/// sync protocol.
///
/// This eliminates the 100ms convergence floor and wasted doc allocation
/// that the full-peer `connect_open` incurs.
pub async fn connect_open_relay(
    socket_path: PathBuf,
    path: PathBuf,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> Result<RelayOpenResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send open handshake
    let handshake = Handshake::OpenNotebook {
        path: path.to_string_lossy().to_string(),
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive connection info
    let info_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let info: NotebookConnectionInfo = serde_json::from_slice(&info_data)?;

    if let Some(ref error) = info.error {
        return Err(SyncError::Protocol(error.clone()));
    }

    let notebook_id = info.notebook_id.clone();
    info!(
        "[relay] Connected to {} (relay mode, no initial sync)",
        notebook_id
    );

    let handle = spawn_relay(notebook_id, frame_tx, reader, writer);

    Ok(RelayOpenResult { handle, info })
}

/// Create a notebook as a relay — transparent byte pipe, no local document.
///
/// Same as `connect_open_relay` but for new notebooks. Performs the
/// CreateNotebook handshake, then immediately starts piping.
#[allow(clippy::too_many_arguments)]
pub async fn connect_create_relay(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<RelayCreateResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send create handshake
    let handshake = Handshake::CreateNotebook {
        runtime: runtime.to_string(),
        working_dir: working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        notebook_id,
        ephemeral: if ephemeral { Some(true) } else { None },
        package_manager,
        dependencies,
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive connection info
    let info_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let info: NotebookConnectionInfo = serde_json::from_slice(&info_data)?;

    if let Some(ref error) = info.error {
        return Err(SyncError::Protocol(error.clone()));
    }

    let notebook_id = info.notebook_id.clone();
    info!(
        "[relay] Created {} (relay mode, no initial sync)",
        notebook_id
    );

    let handle = spawn_relay(notebook_id, frame_tx, reader, writer);

    Ok(RelayCreateResult { handle, info })
}

/// Connect to a notebook room by ID as a relay — no local document.
///
/// Same as `connect_open_relay` but for connecting to an existing room
/// by notebook ID rather than file path. Used by integration tests.
pub async fn connect_relay(
    socket_path: PathBuf,
    notebook_id: String,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> Result<RelayConnectResult, SyncError> {
    let stream = connect_stream!(&socket_path);
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send preamble
    connection::send_preamble(&mut writer).await?;

    // Send notebook sync handshake
    let handshake = Handshake::NotebookSync {
        notebook_id: notebook_id.clone(),
        protocol: Some(PROTOCOL_V4.to_string()),
        initial_metadata: None,
        working_dir: None,
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive protocol capabilities (v4 handshake)
    let caps_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let caps: ProtocolCapabilities = serde_json::from_slice(&caps_data)
        .map_err(|e| SyncError::Protocol(format!("Parse capabilities: {}", e)))?;
    check_daemon_protocol_version(&caps);

    info!(
        "[relay] Connected to {} (relay mode, no initial sync)",
        notebook_id
    );

    let handle = spawn_relay(notebook_id, frame_tx, reader, writer);

    Ok(RelayConnectResult { handle })
}

/// Result of connecting to a notebook room by ID as a relay.
pub struct RelayConnectResult {
    /// Handle for forwarding frames and sending requests.
    pub handle: RelayHandle,
}

/// Spawn a relay task and return the handle.
///
/// Common tail for `connect_open_relay` and `connect_create_relay`.
fn spawn_relay<R, W>(
    notebook_id: String,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
    reader: R,
    writer: W,
) -> RelayHandle
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<crate::relay::RelayCommand>(32);

    let handle = RelayHandle::new(cmd_tx, notebook_id.clone());

    let task_config = relay_task::RelayTaskConfig {
        cmd_rx,
        frame_tx,
        notebook_id: notebook_id.clone(),
    };

    tokio::spawn(async move {
        relay_task::run(task_config, reader, writer).await;
    });

    handle
}

/// Log version info from a daemon's `ProtocolCapabilities` response.
///
/// Warns on protocol version mismatch but does not error — the preamble
/// already hard-rejects incompatible protocol versions, so any connection
/// that gets this far has a matching wire format. This check surfaces
/// version differences for debugging (e.g., a daemon rebuilt from a
/// different commit).
fn check_daemon_protocol_version(caps: &ProtocolCapabilities) {
    let expected = notebook_protocol::connection::PROTOCOL_VERSION;

    if let Some(remote) = caps.protocol_version {
        if remote != expected {
            log::warn!(
                "[notebook-sync] Daemon protocol version ({}) differs from client ({}). \
                 This connection may behave unexpectedly.",
                remote,
                expected,
            );
        }
    }

    if let Some(ref ver) = caps.daemon_version {
        debug!("[notebook-sync] Connected to daemon version {}", ver);
    }
}
