//! Sync task — background network I/O loop.
//!
//! The sync task owns the socket connection to the daemon and handles:
//!
//! 1. **Local changes** — when `DocHandle::with_doc` mutates the document,
//!    it sends a notification via `changed_rx`. The sync task generates an
//!    Automerge sync message and sends it to the daemon.
//!
//! 2. **Remote changes** — when the daemon sends sync messages (from other
//!    peers), the sync task applies them to the shared document and publishes
//!    a new snapshot.
//!
//! 3. **Protocol operations** — daemon request/response (`SendRequest`),
//!    sync confirmation (`ConfirmSync`), and presence frames still go through
//!    a command channel since they need socket I/O.
//!
//! Document mutations do NOT go through this task. Callers mutate directly
//! via `DocHandle::with_doc`. This task is purely for network synchronization.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;

/// Commands that require socket I/O (not document mutations).
///
/// This is intentionally minimal — only operations that need the network
/// connection go through this channel. Document mutations happen directly
/// on the `DocHandle` via `with_doc`.
pub enum SyncCommand {
    // TODO: SendRequest, ConfirmSync, SendPresence, ReceiveFrontendSyncMessage
    //
    // These will be ported from the existing NotebookSyncClient as the
    // migration progresses. Each carries a oneshot reply channel for the
    // caller to await the result.
}

/// Configuration for the sync task.
pub struct SyncTaskConfig {
    /// Shared document state (same Arc as DocHandle).
    pub doc: Arc<Mutex<SharedDocState>>,

    /// Receives notifications when the document was mutated locally.
    /// The sync task should generate and send a sync message to the daemon.
    pub changed_rx: mpsc::UnboundedReceiver<()>,

    /// Receives protocol commands (request/response, confirm_sync, presence).
    pub cmd_rx: mpsc::Receiver<SyncCommand>,

    /// Watch sender for publishing snapshots after applying remote changes.
    pub snapshot_tx: Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
}

/// Run the sync task.
///
/// This is spawned as a background tokio task. It runs until the socket
/// closes or all handles are dropped (channels close).
///
/// # Architecture
///
/// ```text
/// loop {
///     select! {
///         // Local doc changed → generate sync message → send to daemon
///         _ = changed_rx.recv() => { ... }
///
///         // Incoming frame from daemon → apply to doc → publish snapshot
///         frame = recv_frame(&mut stream) => { ... }
///
///         // Protocol command (request/response, etc.)
///         cmd = cmd_rx.recv() => { ... }
///     }
/// }
/// ```
///
/// The document mutex is held briefly for sync message generation/application.
/// It is NEVER held across `.await` points (socket I/O).
pub async fn run<S>(_config: SyncTaskConfig, _stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // TODO: Port the sync loop from notebook_sync_client.rs::run_sync_task.
    //
    // The key difference from the current implementation:
    //
    // 1. No document mutation commands to handle (those happen on DocHandle)
    // 2. The `changed_rx` channel replaces the mutation commands —
    //    when it fires, lock the doc, generate_sync_message, send to daemon
    // 3. Incoming frames: lock the doc, receive_sync_message, publish snapshot
    // 4. Protocol commands (SendRequest etc.): same as current implementation
    //
    // The select! loop structure:
    //
    // loop {
    //     select! {
    //         // Drain all pending change notifications (coalesce)
    //         _ = changed_rx.recv() => {
    //             // Drain any additional notifications (coalesce multiple mutations)
    //             while changed_rx.try_recv().is_ok() {}
    //             let mut state = config.doc.lock().unwrap();
    //             if let Some(msg) = state.generate_sync_message() {
    //                 let msg_bytes = msg.encode();
    //                 drop(state);  // release before I/O
    //                 send_sync_frame(&mut stream, &msg_bytes).await;
    //             }
    //         }
    //
    //         // Incoming frame from daemon
    //         frame = recv_frame(&mut stream) => {
    //             match frame {
    //                 AutomergeSync(payload) => {
    //                     let msg = sync::Message::decode(&payload)?;
    //                     let mut state = config.doc.lock().unwrap();
    //                     state.receive_sync_message(msg)?;
    //                     let snapshot = NotebookSnapshot::from_doc(&state.doc);
    //                     drop(state);
    //                     config.snapshot_tx.send(snapshot);
    //                     // Generate response sync message if needed
    //                     ...
    //                 }
    //                 Broadcast(payload) => { ... }
    //                 ...
    //             }
    //         }
    //
    //         // Protocol commands
    //         cmd = cmd_rx.recv() => { ... }
    //     }
    // }

    log::info!("[notebook-sync] Sync task stub — not yet implemented");
}
