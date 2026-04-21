//! `DocHandle` — direct, synchronous access to the Automerge document.
//!
//! Inspired by [samod](https://github.com/alexjg/samod)'s `DocHandle`, this
//! provides callers with a `with_doc` method that locks the shared document,
//! runs a closure, publishes a snapshot, and notifies the sync task.
//!
//! Document mutations are synchronous and microsecond-fast. Only daemon
//! protocol operations (`send_request`, `confirm_sync`) are async.
//!
//! ## Convenience methods vs `with_doc`
//!
//! For single operations, use the convenience methods (`add_cell_after`,
//! `update_source`, `set_metadata_string`, etc.). For compound operations
//! that should be atomic (one lock, one snapshot, one sync), use `with_doc`
//! directly:
//!
//! ```ignore
//! // Single operation — convenience method
//! handle.add_cell_after("cell-1", "code", None)?;
//!
//! // Compound operation — with_doc for atomicity
//! handle.with_doc(|doc| {
//!     let mut nd = NotebookDoc::wrap(std::mem::take(doc));
//!     nd.add_cell_after("cell-1", "code", None)?;
//!     nd.update_source("cell-1", "print('hello')")?;
//!     nd.set_cell_source_hidden("cell-1", true)?;
//!     *doc = nd.into_inner();
//!     Ok(())
//! })?;
//! ```

use std::sync::{Arc, Mutex};

use automerge::{AutoCommit, ReadDoc, Value};
use log::debug;
use tokio::sync::{mpsc, oneshot, watch};

use notebook_doc::runtime_state::RuntimeState;
use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

use crate::error::SyncError;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;
use crate::sync_task::SyncCommand;

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
/// handle.add_cell_after("cell-1", "code", None)?;
/// handle.update_source("cell-1", "print('hello')")?;
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

    /// Command channel for async operations (request/response, confirm_sync, presence).
    cmd_tx: mpsc::Sender<SyncCommand>,

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
        cmd_tx: mpsc::Sender<SyncCommand>,
        snapshot_tx: Arc<watch::Sender<NotebookSnapshot>>,
        snapshot_rx: watch::Receiver<NotebookSnapshot>,
        notebook_id: String,
    ) -> Self {
        Self {
            doc,
            changed_tx,
            cmd_tx,
            snapshot_tx,
            snapshot_rx,
            notebook_id,
        }
    }

    /// The notebook ID this handle is connected to.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Set the actor identity for this handle's Automerge document.
    ///
    /// Tags all subsequent edits with the given label for provenance tracking
    /// (e.g., `"agent:claude"`, `"runtimed-py:<session>"`).
    pub fn set_actor(&self, actor_label: &str) -> Result<(), SyncError> {
        let mut state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;
        state
            .doc
            .set_actor(automerge::ActorId::from(actor_label.as_bytes()));
        Ok(())
    }

    /// Get the actor identity label for this handle's document.
    pub fn get_actor_id(&self) -> Result<String, SyncError> {
        let state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;
        Ok(notebook_doc::actor_label_from_id(state.doc.get_actor()))
    }

    /// Read the current runtime state from the synced RuntimeStateDoc.
    ///
    /// Returns the latest snapshot of kernel status, queue, env sync,
    /// and last_saved as seen by this client's Automerge replica.
    pub fn get_runtime_state(&self) -> Result<RuntimeState, SyncError> {
        let state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;
        Ok(state.state_doc.read_state())
    }

    // =====================================================================
    // Document mutations — synchronous, direct, no channels
    // =====================================================================

    /// Mutate the document directly via a closure.
    ///
    /// This is the primary mutation API. The closure receives a mutable
    /// `&mut AutoCommit` reference and can perform any document operations.
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
    /// `add_cell_after`, `set_metadata_string`) which handle the wrap/unwrap
    /// internally. Use `with_doc` for custom or compound operations.
    pub fn with_doc<F, R>(&self, f: F) -> Result<R, SyncError>
    where
        F: FnOnce(&mut AutoCommit) -> R,
    {
        let mut state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;

        let result = f(&mut state.doc);

        debug!("[doc-handle] mutation applied ({})", self.notebook_id);

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

    /// Read the document without publishing a snapshot or notifying the sync task.
    ///
    /// Use this for read-only operations (e.g., `get_metadata_string`) that
    /// don't mutate the document and therefore shouldn't trigger a sync cycle.
    fn with_doc_readonly<F, R>(&self, f: F) -> Result<R, SyncError>
    where
        F: FnOnce(&AutoCommit) -> R,
    {
        let state = self.doc.lock().map_err(|_| SyncError::LockPoisoned)?;
        Ok(f(&state.doc))
    }

    // =====================================================================
    // Convenience methods — single-operation wrappers around with_doc
    // =====================================================================

    // Helper: run a closure on a NotebookDoc wrapper, handling the
    // wrap/unwrap dance and error type conversion.
    fn with_notebook_doc<F, T>(&self, f: F) -> Result<T, SyncError>
    where
        F: FnOnce(&mut notebook_doc::NotebookDoc) -> Result<T, automerge::AutomergeError>,
    {
        self.with_doc(|doc| {
            let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
            let result = f(&mut nd);
            *doc = nd.into_inner();
            result.map_err(SyncError::Automerge)
        })?
    }

    /// Add a new cell after the given cell (or at the beginning if `None`).
    ///
    /// Returns the fractional position string assigned to the cell.
    pub fn add_cell_after(
        &self,
        cell_id: &str,
        cell_type: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, SyncError> {
        self.with_notebook_doc(|nd| nd.add_cell_after(cell_id, cell_type, after_cell_id))
    }

    /// Add a new cell with source in a single atomic transaction.
    ///
    /// Prevents peers from seeing an empty cell before the source arrives.
    /// Both the cell structure and source are written in one lock acquisition,
    /// one snapshot publish, and one sync notification.
    pub fn add_cell_with_source(
        &self,
        cell_id: &str,
        cell_type: &str,
        after_cell_id: Option<&str>,
        source: &str,
    ) -> Result<String, SyncError> {
        self.with_notebook_doc(|nd| {
            let position = nd.add_cell_after(cell_id, cell_type, after_cell_id)?;
            nd.update_source(cell_id, source)?;
            Ok(position)
        })
    }

    /// Delete a cell by ID. Returns true if found and deleted.
    pub fn delete_cell(&self, cell_id: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.delete_cell(cell_id))
    }

    /// Move a cell to after another cell (or to the beginning if `None`).
    /// Returns the new position string.
    pub fn move_cell(
        &self,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, SyncError> {
        self.with_notebook_doc(|nd| nd.move_cell(cell_id, after_cell_id))
    }

    /// Update a cell's source text. Returns true if cell was found.
    pub fn update_source(&self, cell_id: &str, source: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.update_source(cell_id, source))
    }

    /// Append text to a cell's source (efficient for streaming tokens). Returns true if cell was found.
    pub fn append_source(&self, cell_id: &str, text: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.append_source(cell_id, text))
    }

    /// Splice a cell's source at a specific position (character-level, no diff).
    /// Deletes `delete_count` characters starting at `index`, then inserts `text`.
    /// Returns true if cell was found.
    pub fn splice_source(
        &self,
        cell_id: &str,
        index: usize,
        delete_count: usize,
        text: &str,
    ) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.splice_source(cell_id, index, delete_count, text))
    }

    /// Set a cell's type. Valid values: "code", "markdown", "raw". Returns true if cell was found.
    pub fn set_cell_type(&self, cell_id: &str, cell_type: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.set_cell_type(cell_id, cell_type))
    }

    /// Set the full notebook metadata snapshot (kernelspec + language_info + runt).
    pub fn set_metadata_snapshot(
        &self,
        snapshot: &notebook_doc::metadata::NotebookMetadataSnapshot,
    ) -> Result<(), SyncError> {
        self.with_notebook_doc(|nd| nd.set_metadata_snapshot(snapshot))
    }

    /// Set cell metadata from a JSON value. Returns true if cell found.
    pub fn set_cell_metadata(
        &self,
        cell_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.set_cell_metadata(cell_id, metadata))
    }

    /// Update cell metadata at a specific path. Returns true if cell found.
    pub fn update_cell_metadata_at(
        &self,
        cell_id: &str,
        path: &[&str],
        value: serde_json::Value,
    ) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.update_cell_metadata_at(cell_id, path, value))
    }

    /// Set whether a cell's source should be hidden.
    pub fn set_cell_source_hidden(&self, cell_id: &str, hidden: bool) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.set_cell_source_hidden(cell_id, hidden))
    }

    /// Set whether a cell's outputs should be hidden.
    pub fn set_cell_outputs_hidden(&self, cell_id: &str, hidden: bool) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.set_cell_outputs_hidden(cell_id, hidden))
    }

    /// Set cell tags.
    pub fn set_cell_tags(&self, cell_id: &str, tags: &[&str]) -> Result<bool, SyncError> {
        let tags_value: Vec<serde_json::Value> = tags
            .iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect();
        self.update_cell_metadata_at(cell_id, &["tags"], serde_json::Value::Array(tags_value))
    }

    /// Set a string metadata value.
    pub fn set_metadata_string(&self, key: &str, value: &str) -> Result<(), SyncError> {
        self.with_notebook_doc(|nd| nd.set_metadata(key, value))
    }

    /// Get a string metadata value.
    pub fn get_metadata_string(&self, key: &str) -> Option<String> {
        self.with_doc_readonly(|doc| {
            let nd = notebook_doc::NotebookDoc::wrap(doc.clone());
            nd.get_metadata(key)
        })
        .ok()
        .flatten()
    }

    /// Add a UV dependency, deduplicating by package name.
    pub fn add_uv_dependency(&self, pkg: &str) -> Result<(), SyncError> {
        self.with_notebook_doc(|nd| nd.add_uv_dependency(pkg))
    }

    /// Remove a UV dependency by package name. Returns true if removed.
    pub fn remove_uv_dependency(&self, pkg: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.remove_uv_dependency(pkg))
    }

    /// Add a Conda dependency, deduplicating by package name.
    pub fn add_conda_dependency(&self, pkg: &str) -> Result<(), SyncError> {
        self.with_notebook_doc(|nd| nd.add_conda_dependency(pkg))
    }

    /// Remove a Conda dependency by package name. Returns true if removed.
    pub fn remove_conda_dependency(&self, pkg: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.remove_conda_dependency(pkg))
    }

    /// Add a Pixi dependency, deduplicating by package name.
    pub fn add_pixi_dependency(&self, pkg: &str) -> Result<(), SyncError> {
        self.with_notebook_doc(|nd| nd.add_pixi_dependency(pkg))
    }

    /// Remove a Pixi dependency by package name. Returns true if removed.
    pub fn remove_pixi_dependency(&self, pkg: &str) -> Result<bool, SyncError> {
        self.with_notebook_doc(|nd| nd.remove_pixi_dependency(pkg))
    }

    /// Get a single cell by ID from the latest snapshot.
    ///
    /// Outputs are not included — they live in `RuntimeStateDoc` keyed by
    /// `execution_id`. Call [`Self::get_cell_outputs`] or
    /// [`Self::get_all_outputs`] alongside this method when you need them.
    pub fn get_cell(&self, cell_id: &str) -> Option<notebook_doc::CellSnapshot> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot.cells.iter().find(|c| c.id == cell_id).cloned()
    }

    /// Get the ordered list of cell IDs from the latest snapshot.
    pub fn get_cell_ids(&self) -> Vec<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot.cells.iter().map(|c| c.id.clone()).collect()
    }

    /// Get the ID of the last cell in document order, or `None` if empty.
    pub fn last_cell_id(&self) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot.cells.last().map(|c| c.id.clone())
    }

    /// Get the ID of the first cell in document order, or `None` if empty.
    pub fn first_cell_id(&self) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot.cells.first().map(|c| c.id.clone())
    }

    /// Get a single cell's source text without cloning the full snapshot.
    pub fn get_cell_source(&self, cell_id: &str) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot
            .cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.source.clone())
    }

    /// Get a single cell's type ("code", "markdown", or "raw").
    pub fn get_cell_type(&self, cell_id: &str) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot
            .cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.cell_type.clone())
    }

    /// Get a single cell's outputs (manifest hashes) from RuntimeStateDoc.
    ///
    /// Reads the cell's `execution_id` from the notebook doc, then looks up
    /// outputs in the RuntimeStateDoc — providing a transparent facade.
    pub fn get_cell_outputs(&self, cell_id: &str) -> Option<Vec<serde_json::Value>> {
        let state = self.doc.lock().ok()?;
        // Read execution_id from the raw Automerge doc
        let eid = read_execution_id(&state.doc, cell_id)?;
        let outputs = state.state_doc.get_outputs(&eid);
        if outputs.is_empty() {
            None
        } else {
            Some(outputs)
        }
    }

    /// Get a single cell's execution count (e.g. "5" or "null").
    pub fn get_cell_execution_count(&self, cell_id: &str) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot
            .cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.execution_count.clone())
    }

    /// Get a single cell's metadata as a JSON value.
    pub fn get_cell_metadata(&self, cell_id: &str) -> Option<serde_json::Value> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot
            .cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.metadata.clone())
    }

    /// Get a single cell's fractional-index position string.
    pub fn get_cell_position(&self, cell_id: &str) -> Option<String> {
        let snapshot = self.snapshot_rx.borrow();
        snapshot
            .cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.position.clone())
    }

    // =====================================================================
    // Async operations — need socket I/O via the sync task
    // =====================================================================

    /// Send a request to the daemon and wait for a response.
    ///
    /// This is async because it involves socket I/O. The request is sent
    /// to the daemon via the sync task, which handles the wire protocol.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let response = handle.send_request(NotebookRequest::LaunchKernel {
    ///     kernel_type: "python".into(),
    ///     env_source: "auto".into(),
    ///     notebook_path: None,
    /// }).await?;
    /// ```
    pub async fn send_request(
        &self,
        request: NotebookRequest,
    ) -> Result<NotebookResponse, SyncError> {
        debug!(
            "[doc-handle] send_request: {:?} ({})",
            std::mem::discriminant(&request),
            self.notebook_id
        );
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SyncCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: None,
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Send a request with a broadcast channel for real-time progress updates.
    ///
    /// Used for long-running requests like `LaunchKernel` where the daemon
    /// sends progress broadcasts (env creation, package installs) while
    /// the request is in flight.
    pub async fn send_request_with_broadcast(
        &self,
        request: NotebookRequest,
        broadcast_tx: tokio::sync::broadcast::Sender<
            notebook_protocol::protocol::NotebookBroadcast,
        >,
    ) -> Result<NotebookResponse, SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SyncCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: Some(broadcast_tx),
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Confirm that the daemon has merged all our local changes.
    ///
    /// Performs up to 5 sync round-trips, checking that the daemon's
    /// shared_heads include our local heads. Call this before executing
    /// a cell to ensure the daemon has the cell's source.
    pub async fn confirm_sync(&self) -> Result<(), SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SyncCommand::ConfirmSync { reply: reply_tx })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Flush pending RuntimeStateDoc sync frames from the daemon.
    ///
    /// Call before reading RuntimeStateDoc state to ensure the local
    /// replica reflects the daemon's latest writes.
    pub async fn confirm_state_sync(&self) -> Result<(), SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SyncCommand::ConfirmStateSync { reply: reply_tx })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Get all connected peer IDs and labels, sorted by peer ID for stable ordering.
    pub fn get_peers(&self) -> Vec<(String, String)> {
        let state = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        let mut peers: Vec<_> = state
            .presence
            .peers()
            .values()
            .map(|p| (p.peer_id.clone(), p.peer_label.clone()))
            .collect();
        peers.sort_by(|a, b| a.0.cmp(&b.0));
        peers
    }

    /// Get all remote peer cursors, excluding the given peer ID.
    ///
    /// Returns `(peer_id, peer_label, cursor_position)` tuples sorted by peer ID.
    pub fn remote_cursors(
        &self,
        exclude_peer: &str,
    ) -> Vec<(String, String, notebook_doc::presence::CursorPosition)> {
        let state = self.doc.lock().unwrap_or_else(|e| e.into_inner());
        let mut cursors: Vec<_> = state
            .presence
            .remote_cursors(exclude_peer)
            .into_iter()
            .map(|(id, pos)| {
                let label = state
                    .presence
                    .peers()
                    .get(id)
                    .map(|p| p.peer_label.clone())
                    .unwrap_or_default();
                (id.to_string(), label, pos.clone())
            })
            .collect();
        cursors.sort_by(|a, b| a.0.cmp(&b.0));
        cursors
    }

    /// Send a raw presence frame to the daemon.
    ///
    /// The daemon relays this to all other peers in the notebook room.
    pub async fn send_presence(&self, data: Vec<u8>) -> Result<(), SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SyncCommand::SendPresence {
                data,
                reply: reply_tx,
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
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
    ///
    /// Outputs are not included — they live in `RuntimeStateDoc` keyed by
    /// `execution_id`. Call [`Self::get_all_outputs`] alongside this method
    /// when you need them.
    pub fn get_cells(&self) -> Vec<notebook_doc::CellSnapshot> {
        self.snapshot_rx.borrow().cells.as_ref().clone()
    }

    /// Get outputs for every cell that currently has an execution_id.
    ///
    /// Outputs are looked up in `RuntimeStateDoc` keyed by `execution_id`.
    /// Cells without an execution_id or with empty outputs are omitted from
    /// the returned map. Prefer this over calling [`Self::get_cell_outputs`]
    /// in a loop — it walks the doc once.
    pub fn get_all_outputs(&self) -> std::collections::HashMap<String, Vec<serde_json::Value>> {
        // Snapshot the cell ids first so we never hold the watch-channel
        // borrow across the shared doc lock. Another task calling `with_doc`
        // acquires the doc lock and then publishes to the watch channel —
        // holding both locks in the reverse order here would deadlock.
        let cell_ids: Vec<String> = self
            .snapshot_rx
            .borrow()
            .cells
            .iter()
            .map(|c| c.id.clone())
            .collect();
        // Poisoned shared state returns empty — matches the silent-fallback
        // pattern used elsewhere in this handle for mutex-locked reads.
        let Ok(state) = self.doc.lock() else {
            return std::collections::HashMap::new();
        };
        cell_ids
            .into_iter()
            .filter_map(|cell_id| {
                let eid = read_execution_id(&state.doc, &cell_id)?;
                let outputs = state.state_doc.get_outputs(&eid);
                (!outputs.is_empty()).then_some((cell_id, outputs))
            })
            .collect()
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
    #[allow(dead_code)]
    pub(crate) fn shared_state(&self) -> &Arc<Mutex<SharedDocState>> {
        &self.doc
    }

    /// Publish a snapshot from the current document state.
    ///
    /// Called by the sync task after applying incoming changes from the daemon.
    /// Handle callers don't need this — `with_doc` publishes automatically.
    #[allow(dead_code)]
    pub(crate) fn publish_snapshot_from_doc(&self, doc: &AutoCommit) {
        let snapshot = NotebookSnapshot::from_doc(doc);
        let _ = self.snapshot_tx.send(snapshot);
    }
}

/// Read the execution_id for a cell directly from a raw AutoCommit document.
fn read_execution_id(doc: &AutoCommit, cell_id: &str) -> Option<String> {
    let (_, cells_id) = doc.get(&automerge::ROOT, "cells").ok().flatten()?;
    let (_, cell_obj) = doc.get(&cells_id, cell_id).ok().flatten()?;
    let (value, _) = doc.get(&cell_obj, "execution_id").ok().flatten()?;
    match value {
        Value::Scalar(s) => match s.as_ref() {
            automerge::ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}
