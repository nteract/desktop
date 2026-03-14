//! `DocHandle` — direct, synchronous access to the Automerge document.
//!
//! Inspired by [samod](https://github.com/alexjg/samod)'s `DocHandle`, this
//! provides callers with a `with_doc` method that locks the shared document,
//! runs a closure, publishes a snapshot, and notifies the sync task.
//!
//! Document mutations are synchronous and microsecond-fast. Only daemon
//! protocol operations (`send_request`, `confirm_sync`) are async.

use std::sync::{Arc, Mutex};

use automerge::AutoCommit;
use tokio::sync::{mpsc, watch};

use crate::error::SyncError;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;

/// A handle to a synced notebook document.
///
/// `DocHandle` is `Clone` — multiple callers can hold handles to the same
/// document. All mutations go through `with_doc`, which acquires the mutex,
/// runs the closure, publishes a snapshot, and notifies the sync task.
///
/// # Example
///
/// ```ignore
/// // Synchronous — no .await needed for document mutations
/// handle.with_doc(|doc| {
///     doc.add_cell(0, "cell-1", "code")?;
///     doc.update_source("cell-1", "print('hello')")?;
///     Ok(())
/// })?;
///
/// // Read the latest snapshot (no lock, no .await)
/// let cells = handle.snapshot().cells();
///
/// // Async — daemon protocol needs socket I/O
/// let response = handle.send_request(NotebookRequest::LaunchKernel { ... }).await?;
/// ```
#[derive(Clone)]
pub struct DocHandle {
    /// Shared document state (doc + sync protocol state).
    /// Both the handle and the sync task hold a reference.
    doc: Arc<Mutex<SharedDocState>>,

    /// Notify the sync task that the document was mutated locally.
    /// The sync task will generate and send a sync message to the daemon.
    changed_tx: mpsc::UnboundedSender<()>,

    /// Watch channel for publishing snapshots after mutations.
    /// The handle publishes; readers (Python API, frontend) subscribe.
    snapshot_tx: Arc<watch::Sender<NotebookSnapshot>>,

    /// Watch channel receiver for reading the latest snapshot.
    snapshot_rx: watch::Receiver<NotebookSnapshot>,

    /// The notebook identifier.
    notebook_id: String,
}

impl std::fmt::Debug for DocHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocHandle")
            .field("notebook_id", &self.notebook_id)
            .finish()
    }
}

impl DocHandle {
    /// Create a new `DocHandle` from shared state and channels.
    ///
    /// This is called by the connection/split logic, not by end users.
    pub(crate) fn new(
        doc: Arc<Mutex<SharedDocState>>,
        changed_tx: mpsc::UnboundedSender<()>,
        snapshot_tx: Arc<watch::Sender<NotebookSnapshot>>,
        snapshot_rx: watch::Receiver<NotebookSnapshot>,
        notebook_id: String,
    ) -> Self {
        Self {
            doc,
            changed_tx,
            snapshot_tx,
            snapshot_rx,
            notebook_id,
        }
    }

    /// The notebook ID this handle is connected to.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    // =====================================================================
    // Document mutations — synchronous, direct, no channels
    // =====================================================================

    /// Mutate the document directly via a closure.
    ///
    /// This is the primary mutation API. The closure receives a mutable
    /// `NotebookDoc` reference and can perform any document operations.
    /// After the closure returns:
    ///
    /// 1. A new snapshot is published (readers see updated state immediately)
    /// 2. The sync task is notified to propagate changes to the daemon
    ///
    /// The mutex is held only for the duration of the closure — keep
    /// mutations fast (microseconds). Never do I/O inside the closure.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::LockPoisoned` if the mutex was poisoned (a thread
    /// panicked while holding it). The closure's own errors are returned via
    /// the `Result` inside `R`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use notebook_doc::NotebookDoc;
    ///
    /// handle.with_doc(|doc| {
    ///     let mut nd = NotebookDoc::wrap(std::mem::take(doc));
    ///     nd.set_metadata_value("runt", &serde_json::json!({
    ///         "uv": { "dependencies": ["pandas>=2.0"] }
    ///     }))?;
    ///     *doc = nd.into_inner();
    ///     Ok(())
    /// })?;
    /// ```
    ///
    /// For convenience, prefer the typed methods on `DocHandle` (e.g.,
    /// `add_cell`, `set_metadata_value`) which handle the wrap/unwrap
    /// internally. Use `with_doc` for custom or compound operations.
    pub fn with_doc<F, R>(&self, f: F) -> Result<R, SyncError>
    where
        F: FnOnce(&mut AutoCommit) -> R,
    {
        let mut state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;

        let result = f(&mut state.doc);

        // Publish a fresh snapshot so readers see the mutation immediately.
        // This happens before the sync task sends it to the daemon — local
        // reads are always up-to-date even if the network is slow.
        let snapshot = NotebookSnapshot::from_doc(&state.doc);
        let _ = self.snapshot_tx.send(snapshot);

        // Notify the sync task that the document changed. The sync task will
        // generate a sync message and send it to the daemon. Unbounded send
        // so we never block the caller. If the sync task is behind, multiple
        // notifications coalesce (it just syncs once).
        let _ = self.changed_tx.send(());

        Ok(result)
    }

    // =====================================================================
    // Read-only access — no lock needed
    // =====================================================================

    /// Get the latest document snapshot.
    ///
    /// Returns the most recent snapshot published after the last mutation.
    /// This reads from a `watch` channel — no mutex lock, no async, instant.
    pub fn snapshot(&self) -> NotebookSnapshot {
        self.snapshot_rx.borrow().clone()
    }

    /// Get all cells from the latest snapshot.
    pub fn get_cells(&self) -> Vec<notebook_doc::CellSnapshot> {
        self.snapshot_rx.borrow().cells.as_ref().clone()
    }

    /// Get the typed notebook metadata from the latest snapshot.
    pub fn get_notebook_metadata(
        &self,
    ) -> Option<notebook_doc::metadata::NotebookMetadataSnapshot> {
        self.snapshot_rx.borrow().notebook_metadata.clone()
    }

    /// Subscribe to snapshot changes.
    ///
    /// Returns a `watch::Receiver` that is notified whenever the document
    /// changes (locally or from a remote peer). Use `.changed().await` to
    /// wait for the next update, then `.borrow()` to read it.
    pub fn subscribe(&self) -> watch::Receiver<NotebookSnapshot> {
        self.snapshot_rx.clone()
    }

    // =====================================================================
    // Direct access to shared state (for the sync task and advanced use)
    // =====================================================================

    /// Get a reference to the shared document state.
    ///
    /// This is primarily for the sync task to apply incoming sync messages.
    /// Callers should prefer `with_doc` for mutations and `snapshot()` for reads.
    pub(crate) fn shared_state(&self) -> &Arc<Mutex<SharedDocState>> {
        &self.doc
    }

    /// Publish a snapshot from the current document state.
    ///
    /// Called by the sync task after applying incoming changes from the daemon.
    /// Handle callers don't need this — `with_doc` publishes automatically.
    pub(crate) fn publish_snapshot_from_doc(&self, doc: &AutoCommit) {
        let snapshot = NotebookSnapshot::from_doc(doc);
        let _ = self.snapshot_tx.send(snapshot);
    }
}
