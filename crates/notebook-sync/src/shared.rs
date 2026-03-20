//! Shared document state behind `Arc<Mutex<_>>`.
//!
//! This is the core state shared between `DocHandle` (callers) and the sync task
//! (network I/O). All document mutations happen through the mutex. The sync task
//! also acquires the mutex briefly to apply incoming sync messages and generate
//! outgoing ones.

use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::AutoCommit;
use notebook_doc::presence::PresenceState;
use notebook_doc::runtime_state::RuntimeStateDoc;

/// The shared state behind `Arc<Mutex<SharedDocState>>`.
///
/// Contains the Automerge document, sync protocol state, and notebook identity.
/// Both the `DocHandle` and the sync task hold `Arc<Mutex<SharedDocState>>`.
pub struct SharedDocState {
    /// The Automerge document — source of truth for all notebook content.
    pub(crate) doc: AutoCommit,

    /// Automerge sync protocol state for the daemon peer.
    pub(crate) peer_state: sync::State,

    /// The notebook identifier (canonical path for file-backed notebooks,
    /// UUID for ephemeral/untitled notebooks).
    pub(crate) notebook_id: String,

    /// Incoming presence state from remote peers (cursors, selections, etc.).
    pub(crate) presence: PresenceState,

    /// Runtime state doc — daemon-authoritative, synced read-only.
    pub(crate) state_doc: RuntimeStateDoc,

    /// Automerge sync protocol state for the RuntimeStateDoc peer.
    pub(crate) state_peer_state: sync::State,
}

impl SharedDocState {
    /// Create a new shared state with the given document and notebook ID.
    pub fn new(doc: AutoCommit, notebook_id: String) -> Self {
        Self {
            doc,
            peer_state: sync::State::new(),
            notebook_id,
            presence: PresenceState::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_peer_state: sync::State::new(),
        }
    }

    /// Get a reference to the notebook ID.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Generate an outgoing sync message for the daemon peer, if any changes
    /// need to be sent.
    ///
    /// Returns `None` if the daemon already has all our changes.
    pub fn generate_sync_message(&mut self) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(&mut self.peer_state)
    }

    /// Apply an incoming sync message from the daemon peer.
    pub fn receive_sync_message(
        &mut self,
        message: sync::Message,
    ) -> Result<(), automerge::AutomergeError> {
        self.doc
            .sync()
            .receive_sync_message(&mut self.peer_state, message)
    }

    // ── RuntimeStateDoc sync ────────────────────────────────────────

    /// Generate an outgoing sync reply for the RuntimeStateDoc.
    pub fn generate_state_sync_message(&mut self) -> Option<sync::Message> {
        self.state_doc
            .doc_mut()
            .sync()
            .generate_sync_message(&mut self.state_peer_state)
    }

    /// Apply an incoming RuntimeStateSync message from the daemon.
    /// No change stripping — the client is a read-only consumer.
    pub fn receive_state_sync_message(
        &mut self,
        message: sync::Message,
    ) -> Result<(), automerge::AutomergeError> {
        self.state_doc
            .doc_mut()
            .sync()
            .receive_sync_message(&mut self.state_peer_state, message)
    }
}
