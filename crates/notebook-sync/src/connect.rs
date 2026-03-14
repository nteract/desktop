//! Connection handshake and initial Automerge sync.
//!
//! Establishes a connection to the runtimed daemon, performs the protocol
//! handshake, and runs the initial Automerge sync exchange to populate
//! the local document replica.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::sync::{self, SyncDoc};
use automerge::AutoCommit;
use log::{debug, info};
use tokio::sync::{mpsc, watch};

use runtimed::connection::{
    self, Handshake, NotebookConnectionInfo, NotebookFrameType, ProtocolCapabilities, PROTOCOL_V2,
};
use runtimed::protocol::NotebookBroadcast;

use crate::error::SyncError;
use crate::handle::DocHandle;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;
use crate::sync_task;

/// Result of connecting to a notebook room.
pub struct ConnectResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: tokio::sync::broadcast::Receiver<NotebookBroadcast>,

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
    pub broadcast_rx: tokio::sync::broadcast::Receiver<NotebookBroadcast>,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,

    /// Initial cells in the document after sync.
    pub cells: Vec<notebook_doc::CellSnapshot>,
}

// TODO: Windows support — use named pipes instead of Unix domain sockets.

/// Connect to a notebook room by ID.
///
/// Performs the protocol handshake and initial Automerge sync. Returns a
/// `DocHandle` for direct document access and a broadcast receiver for
/// kernel events.
#[cfg(unix)]
pub async fn connect(
    socket_path: PathBuf,
    notebook_id: String,
) -> Result<ConnectResult, SyncError> {
    connect_with_options(socket_path, notebook_id, None, None).await
}

/// Connect to a notebook room with options.
#[cfg(unix)]
pub async fn connect_with_options(
    socket_path: PathBuf,
    notebook_id: String,
    working_dir: Option<PathBuf>,
    initial_metadata: Option<String>,
) -> Result<ConnectResult, SyncError> {
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .map_err(SyncError::Io)?;

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
    let _caps: ProtocolCapabilities = serde_json::from_slice(&caps_data)?;

    // Initial Automerge sync exchange
    let mut doc = AutoCommit::new();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
    )
    .await?;

    info!(
        "[notebook-sync] Connected to room {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    // Read initial state before splitting
    let cells = notebook_doc::get_cells_from_doc(&doc);
    let legacy_metadata =
        notebook_doc::get_metadata_from_doc(&doc, notebook_doc::metadata::NOTEBOOK_METADATA_KEY);

    // Build the shared state and channels
    let shared = Arc::new(Mutex::new(SharedDocState::new(doc, notebook_id.clone())));
    // Restore the peer_state from the handshake sync
    {
        let mut state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        state.peer_state = peer_state;
    }

    let initial_snapshot = {
        let state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        NotebookSnapshot::from_doc(&state.doc)
    };

    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<sync_task::SyncCommand>(32);
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(64);
    let cmd_tx_for_handle = cmd_tx.clone();

    // Send any broadcasts received during initial sync
    for bc in pending_broadcasts {
        let _ = broadcast_tx.send(bc);
    }

    // Build the handle
    let handle = DocHandle::new(
        Arc::clone(&shared),
        changed_tx,
        cmd_tx_for_handle,
        Arc::clone(&snapshot_tx),
        snapshot_rx,
        notebook_id.clone(),
    );

    // Reunite the split stream for the sync task
    let stream = reader.into_inner().unsplit(writer.into_inner());

    // Spawn the sync task
    let task_config = sync_task::SyncTaskConfig {
        doc: Arc::clone(&shared),
        changed_rx,
        cmd_rx,
        snapshot_tx: Arc::clone(&snapshot_tx),
        broadcast_tx,
    };

    let notebook_id_for_task = notebook_id.clone();
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Sync task started for {}",
            notebook_id_for_task
        );
        sync_task::run(task_config, stream).await;
        info!(
            "[notebook-sync] Sync task stopped for {}",
            notebook_id_for_task
        );
    });

    Ok(ConnectResult {
        handle,
        broadcast_rx,
        cells,
        initial_metadata: legacy_metadata,
    })
}

/// Connect and open an existing notebook file.
#[cfg(unix)]
pub async fn connect_open(socket_path: PathBuf, path: PathBuf) -> Result<OpenResult, SyncError> {
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .map_err(SyncError::Io)?;

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

    // Initial Automerge sync exchange
    let mut doc = AutoCommit::new();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
    )
    .await?;

    info!(
        "[notebook-sync] Opened notebook {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    let cells = notebook_doc::get_cells_from_doc(&doc);

    // Build shared state and channels
    let shared = Arc::new(Mutex::new(SharedDocState::new(doc, notebook_id.clone())));
    {
        let mut state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        state.peer_state = peer_state;
    }

    let initial_snapshot = {
        let state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        NotebookSnapshot::from_doc(&state.doc)
    };

    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<sync_task::SyncCommand>(32);
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(64);
    let cmd_tx_for_handle = cmd_tx.clone();

    for bc in pending_broadcasts {
        let _ = broadcast_tx.send(bc);
    }

    let handle = DocHandle::new(
        Arc::clone(&shared),
        changed_tx,
        cmd_tx_for_handle,
        Arc::clone(&snapshot_tx),
        snapshot_rx,
        notebook_id.clone(),
    );

    let stream = reader.into_inner().unsplit(writer.into_inner());

    let task_config = sync_task::SyncTaskConfig {
        doc: Arc::clone(&shared),
        changed_rx,
        cmd_rx,
        snapshot_tx: Arc::clone(&snapshot_tx),
        broadcast_tx,
    };

    let notebook_id_for_task = notebook_id.clone();
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Sync task started for {}",
            notebook_id_for_task
        );
        sync_task::run(task_config, stream).await;
        info!(
            "[notebook-sync] Sync task stopped for {}",
            notebook_id_for_task
        );
    });

    Ok(OpenResult {
        handle,
        broadcast_rx,
        info,
        cells,
    })
}

/// Result of creating a new notebook.
pub struct CreateResult {
    /// Handle for document mutations and reads.
    pub handle: DocHandle,

    /// Receiver for kernel/execution broadcasts from the daemon.
    pub broadcast_rx: tokio::sync::broadcast::Receiver<NotebookBroadcast>,

    /// Connection info from the daemon (notebook_id, trust status, etc).
    pub info: NotebookConnectionInfo,

    /// Initial cells in the document after sync.
    pub cells: Vec<notebook_doc::CellSnapshot>,
}

/// Connect and create a new notebook.
///
/// The daemon creates an empty notebook room with one code cell and
/// returns connection info with a generated UUID as the notebook_id.
#[cfg(unix)]
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
) -> Result<CreateResult, SyncError> {
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .map_err(SyncError::Io)?;

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
        notebook_id: None,
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

    // Initial Automerge sync exchange
    let mut doc = AutoCommit::new();
    let mut peer_state = sync::State::new();
    let mut pending_broadcasts = Vec::new();

    do_initial_sync(
        &mut reader,
        &mut writer,
        &mut doc,
        &mut peer_state,
        &mut pending_broadcasts,
    )
    .await?;

    info!(
        "[notebook-sync] Created notebook {} ({} cells)",
        notebook_id,
        notebook_doc::get_cells_from_doc(&doc).len()
    );

    let cells = notebook_doc::get_cells_from_doc(&doc);

    // Build shared state and channels
    let shared = Arc::new(Mutex::new(SharedDocState::new(doc, notebook_id.clone())));
    {
        let mut state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        state.peer_state = peer_state;
    }

    let initial_snapshot = {
        let state = shared.lock().map_err(|_| SyncError::LockPoisoned)?;
        NotebookSnapshot::from_doc(&state.doc)
    };

    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<sync_task::SyncCommand>(32);
    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel::<NotebookBroadcast>(64);
    let cmd_tx_for_handle = cmd_tx.clone();

    for bc in pending_broadcasts {
        let _ = broadcast_tx.send(bc);
    }

    let handle = DocHandle::new(
        Arc::clone(&shared),
        changed_tx,
        cmd_tx_for_handle,
        Arc::clone(&snapshot_tx),
        snapshot_rx,
        notebook_id.clone(),
    );

    let stream = reader.into_inner().unsplit(writer.into_inner());

    let task_config = sync_task::SyncTaskConfig {
        doc: Arc::clone(&shared),
        changed_rx,
        cmd_rx,
        snapshot_tx: Arc::clone(&snapshot_tx),
        broadcast_tx,
    };

    let notebook_id_for_task = notebook_id.clone();
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Sync task started for {}",
            notebook_id_for_task
        );
        sync_task::run(task_config, stream).await;
        info!(
            "[notebook-sync] Sync task stopped for {}",
            notebook_id_for_task
        );
    });

    Ok(CreateResult {
        handle,
        broadcast_rx,
        info,
        cells,
    })
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
) -> Result<(), SyncError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
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
