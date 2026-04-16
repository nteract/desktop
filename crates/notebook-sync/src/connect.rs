//! Connection handshake and initial Automerge sync.
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
//! handshake, and runs the initial Automerge sync exchange to populate
//! the local document replica.
//!
//! Platform-specific stream creation (Unix socket or Windows named pipe)
//! is handled internally. The handshake and sync logic is generic over
//! `AsyncRead + AsyncWrite`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::sync::{self, SyncDoc};
use automerge::AutoCommit;
use log::{debug, info};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use notebook_protocol::connection::{
    self, Handshake, NotebookConnectionInfo, NotebookFrameType, ProtocolCapabilities, PROTOCOL_V2,
};
use notebook_protocol::protocol::NotebookBroadcast;

use crate::error::SyncError;
use crate::handle::DocHandle;
use crate::relay::RelayHandle;
use crate::relay_task;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;
use crate::sync_task;

/// Result of connecting to a notebook room.
pub struct ConnectResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: crate::BroadcastReceiver,

    /// Initial cells in the document after sync.
    pub cells: Vec<notebook_doc::CellSnapshot>,

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

    /// Initial cells in the document after sync.
    pub cells: Vec<notebook_doc::CellSnapshot>,
}

/// Result of creating a new notebook.
pub struct CreateResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: crate::BroadcastReceiver,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,

    /// Initial cells in the document after sync.
    pub cells: Vec<notebook_doc::CellSnapshot>,
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
/// Performs the protocol handshake and initial Automerge sync. Returns a
/// `DocHandle` for direct document access and a broadcast receiver for
/// kernel events.
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
        protocol: Some(PROTOCOL_V2.to_string()),
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

    // Initial Automerge sync exchange — start from the standard notebook
    // skeleton so load_incremental takes the incremental path.
    // Set the actor before sync so all ops (including bootstrap) are
    // attributed to the caller, not a random UUID.
    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let mut doc = bootstrap.into_inner();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();
    let mut pending_state_sync_frames = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
        &mut pending_state_sync_frames,
    )
    .await?;

    info!(
        "[notebook-sync] Connected to room {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    // Read initial state before splitting
    let cells = notebook_doc::get_cells_from_doc(&doc);
    let initial_metadata_snapshot = notebook_doc::get_metadata_snapshot_from_doc(&doc)
        .and_then(|s| serde_json::to_string(&s).ok());

    // Build the shared state and channels
    build_and_spawn(
        doc,
        peer_state,
        notebook_id,
        pending_broadcasts,
        pending_state_sync_frames,
        reader,
        writer,
    )
    .await
    .map(|(handle, broadcast_rx)| ConnectResult {
        handle,
        broadcast_rx,
        cells,
        initial_metadata: initial_metadata_snapshot,
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

    // Initial Automerge sync exchange — start from the standard notebook
    // skeleton so load_incremental takes the incremental path.
    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let mut doc = bootstrap.into_inner();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();
    let mut pending_state_sync_frames = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
        &mut pending_state_sync_frames,
    )
    .await?;

    info!(
        "[notebook-sync] Opened notebook {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    let cells = notebook_doc::get_cells_from_doc(&doc);

    build_and_spawn(
        doc,
        peer_state,
        notebook_id,
        pending_broadcasts,
        pending_state_sync_frames,
        reader,
        writer,
    )
    .await
    .map(|(handle, broadcast_rx)| OpenResult {
        handle,
        broadcast_rx,
        info,
        cells,
    })
}

/// Connect and create a new notebook.
///
/// The daemon creates an empty notebook room with one code cell and
/// returns connection info with a generated UUID as the notebook_id.
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: &str,
    ephemeral: bool,
) -> Result<CreateResult, SyncError> {
    connect_create_inner(
        socket_path,
        runtime,
        working_dir,
        None,
        actor_label,
        ephemeral,
    )
    .await
}

async fn connect_create_inner(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    actor_label: &str,
    ephemeral: bool,
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

    // Initial Automerge sync exchange — start from the standard notebook
    // skeleton so load_incremental takes the incremental path.
    let bootstrap = notebook_doc::NotebookDoc::bootstrap(
        notebook_doc::TextEncoding::UnicodeCodePoint,
        actor_label,
    );
    let mut doc = bootstrap.into_inner();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();
    let mut pending_state_sync_frames = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
        &mut pending_state_sync_frames,
    )
    .await?;

    info!(
        "[notebook-sync] Created notebook {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    let cells = notebook_doc::get_cells_from_doc(&doc);

    build_and_spawn(
        doc,
        peer_state,
        notebook_id,
        pending_broadcasts,
        pending_state_sync_frames,
        reader,
        writer,
    )
    .await
    .map(|(handle, broadcast_rx)| CreateResult {
        handle,
        broadcast_rx,
        info,
        cells,
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
    pending_broadcasts: Vec<NotebookBroadcast>,
    pending_state_sync_frames: Vec<Vec<u8>>,
    mut reader: R,
    mut writer: W,
) -> Result<(DocHandle, crate::BroadcastReceiver), SyncError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut shared_state = SharedDocState::new(doc, notebook_id.clone());
    shared_state.peer_state = peer_state;

    // Complete the RuntimeStateDoc sync handshake inline so the doc is
    // fully populated before we return the handle. The Automerge sync
    // protocol is multi-round: the daemon's initial message contains only
    // heads/bloom; the actual document data arrives after we reply.
    //
    // 1. Apply buffered frames from do_initial_sync
    // 2. Send reply messages to the daemon
    // 3. Receive follow-up frames until convergence (100ms timeout)
    {
        // Step 1: Apply buffered frames
        for frame_payload in &pending_state_sync_frames {
            if let Ok(msg) = sync::Message::decode(frame_payload) {
                let _ = shared_state.receive_state_sync_message(msg);
            }
        }

        // Step 2: Send replies
        while let Some(reply) = shared_state.generate_state_sync_message() {
            connection::send_typed_frame(
                &mut writer,
                NotebookFrameType::RuntimeStateSync,
                &reply.encode(),
            )
            .await?;
        }

        // Step 3: Receive follow-up frames until convergence
        loop {
            match tokio::time::timeout(
                Duration::from_millis(100),
                connection::recv_typed_frame(&mut reader),
            )
            .await
            {
                Ok(Ok(Some(frame))) if frame.frame_type == NotebookFrameType::RuntimeStateSync => {
                    if let Ok(msg) = sync::Message::decode(&frame.payload) {
                        let _ = shared_state.receive_state_sync_message(msg);
                    }
                    // Reply to each round
                    while let Some(reply) = shared_state.generate_state_sync_message() {
                        connection::send_typed_frame(
                            &mut writer,
                            NotebookFrameType::RuntimeStateSync,
                            &reply.encode(),
                        )
                        .await?;
                    }
                }
                Ok(Ok(Some(_frame))) => {
                    // Non-RuntimeStateSync frame — sync has converged,
                    // but we received a different frame type (e.g. broadcast).
                    // We can't put it back, so break and let the sync task
                    // handle subsequent frames.
                    break;
                }
                Ok(Ok(None)) => {
                    return Err(SyncError::Protocol(
                        "Connection closed during RuntimeStateDoc sync".into(),
                    ));
                }
                Ok(Err(e)) => {
                    return Err(SyncError::Io(e));
                }
                Err(_) => {
                    // Timeout — RuntimeStateDoc sync converged
                    break;
                }
            }
        }
    }

    let shared = Arc::new(Mutex::new(shared_state));

    let initial_snapshot = {
        let state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        NotebookSnapshot::from_doc(&state.doc)
    };

    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<sync_task::SyncCommand>(32);
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(64);

    // Send any broadcasts received during initial sync
    for bc in pending_broadcasts {
        let _ = broadcast_tx.send(bc);
    }

    let handle = DocHandle::new(
        Arc::clone(&shared),
        changed_tx,
        cmd_tx,
        Arc::clone(&snapshot_tx),
        snapshot_rx,
        notebook_id.clone(),
    );

    let task_config = sync_task::SyncTaskConfig {
        doc: Arc::clone(&shared),
        changed_rx,
        cmd_rx,
        snapshot_tx: Arc::clone(&snapshot_tx),
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
/// Does NOT call `do_initial_sync` — the daemon's initial sync message
/// stays in the socket buffer and gets piped to the frontend by the relay
/// task. The frontend (WASM) owns the sync protocol.
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
pub async fn connect_create_relay(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
    ephemeral: bool,
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
        protocol: Some(PROTOCOL_V2.to_string()),
        initial_metadata: None,
        working_dir: None,
    };
    connection::send_json_frame(&mut writer, &handshake)
        .await
        .map_err(|e| SyncError::Protocol(format!("Send handshake: {}", e)))?;

    // Receive protocol capabilities (v2 handshake)
    let caps_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;
    let caps: ProtocolCapabilities = serde_json::from_slice(&caps_data)
        .map_err(|e| SyncError::Protocol(format!("Parse capabilities: {}", e)))?;
    check_daemon_protocol_version(&caps);

    // Receive initial metadata frame (may be empty)
    let _initial_data = connection::recv_frame(&mut reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during handshake".into()))?;

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

/// Perform the initial Automerge sync exchange after handshake.
///
/// Exchanges sync messages with the daemon until the local document is
/// caught up. Also buffers any broadcasts received during sync.
async fn do_initial_sync<R, W>(
    reader: &mut R,
    writer: &mut W,
    doc: &mut AutoCommit,
    peer_state: &mut sync::State,
    pending_broadcasts: &mut Vec<NotebookBroadcast>,
    pending_state_sync_frames: &mut Vec<Vec<u8>>,
) -> Result<(), SyncError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Receive the daemon's first sync message
    let first_frame = connection::recv_typed_frame(reader)
        .await?
        .ok_or_else(|| SyncError::Protocol("Connection closed during initial sync".into()))?;

    if first_frame.frame_type != NotebookFrameType::AutomergeSync {
        return Err(SyncError::Protocol(format!(
            "Expected AutomergeSync frame, got {:?}",
            first_frame.frame_type
        )));
    }

    // Apply and respond
    let msg = sync::Message::decode(&first_frame.payload)
        .map_err(|e| SyncError::Protocol(format!("Decode sync message: {}", e)))?;
    doc.sync()
        .receive_sync_message(peer_state, msg)
        .map_err(|e| SyncError::Protocol(format!("Apply sync message: {}", e)))?;

    // Generate reply
    if let Some(reply) = doc.sync().generate_sync_message(peer_state) {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &reply.encode())
            .await?;
    }

    // Continue receiving until we hit a timeout (convergence)
    let mut rounds = 0;
    loop {
        match tokio::time::timeout(
            Duration::from_millis(100),
            connection::recv_typed_frame(reader),
        )
        .await
        {
            Ok(Ok(Some(frame))) => match frame.frame_type {
                NotebookFrameType::AutomergeSync => {
                    let msg = sync::Message::decode(&frame.payload)
                        .map_err(|e| SyncError::Protocol(format!("Decode sync: {}", e)))?;
                    doc.sync()
                        .receive_sync_message(peer_state, msg)
                        .map_err(|e| SyncError::Protocol(format!("Apply sync: {}", e)))?;

                    if let Some(reply) = doc.sync().generate_sync_message(peer_state) {
                        connection::send_typed_frame(
                            writer,
                            NotebookFrameType::AutomergeSync,
                            &reply.encode(),
                        )
                        .await?;
                    }
                    rounds += 1;
                }
                NotebookFrameType::Broadcast => {
                    // Buffer broadcasts received during initial sync
                    if let Ok(bc) = serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                        pending_broadcasts.push(bc);
                    }
                }
                NotebookFrameType::RuntimeStateSync => {
                    // Buffer RuntimeStateSync frames — they'll be applied to
                    // SharedDocState after it's created (do_initial_sync only
                    // has the notebook doc, not the RuntimeStateDoc).
                    pending_state_sync_frames.push(frame.payload);
                }
                _ => {
                    debug!(
                        "[notebook-sync] Ignoring {:?} frame during initial sync",
                        frame.frame_type
                    );
                }
            },
            Ok(Ok(None)) => {
                return Err(SyncError::Protocol(
                    "Connection closed during initial sync".into(),
                ));
            }
            Ok(Err(e)) => {
                return Err(SyncError::Io(e));
            }
            Err(_) => {
                // Timeout — sync converged
                debug!(
                    "[notebook-sync] Initial sync converged after {} rounds",
                    rounds
                );
                break;
            }
        }
    }

    Ok(())
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
