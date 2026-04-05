//! PoolDoc — global daemon-authoritative Automerge document for pool state.
//!
//! One per daemon (not per notebook). Describes the UV and Conda prewarm pool
//! status: available environments, warming count, pool size, and error state.
//! Clients sync read-only via the Automerge sync protocol — the daemon strips
//! any client-side changes.
//!
//! Schema:
//! ```text
//! ROOT/
//!   uv/
//!     available: u64
//!     warming: u64
//!     pool_size: u64
//!     consecutive_failures: u64
//!     retry_in_secs: u64
//!     error: Str (optional — deleted when None)
//!   conda/
//!     available: u64
//!     warming: u64
//!     pool_size: u64
//!     consecutive_failures: u64
//!     retry_in_secs: u64
//!     error: Str (optional — deleted when None)
//! ```

use automerge::{
    sync, sync::SyncDoc, transaction::Transactable, ActorId, AutoCommit, AutomergeError, ObjType,
    ReadDoc, Value, ROOT,
};
use serde::{Deserialize, Serialize};

// ── Snapshot types ───────────────────────────────────────────────────

/// State of a single runtime pool (UV or Conda).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePoolState {
    pub available: u64,
    pub warming: u64,
    pub pool_size: u64,
    /// Human-readable error message (None if healthy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Package that failed to install (None if not identified or healthy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_package: Option<String>,
    /// Number of consecutive failures (0 if healthy).
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Seconds until next retry (0 if retry is imminent or healthy).
    #[serde(default)]
    pub retry_in_secs: u64,
}

/// Full pool state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolState {
    pub uv: RuntimePoolState,
    pub conda: RuntimePoolState,
    pub pixi: RuntimePoolState,
}

// ── PoolDoc ─────────────────────────────────────────────────────────

/// Global daemon-authoritative Automerge document for pool state.
///
/// The daemon creates one of these on startup. Clients receive updates
/// via Automerge sync — never by direct mutation.
pub struct PoolDoc {
    doc: AutoCommit,
    /// Cached last-written state to avoid redundant Automerge writes
    /// when the pool state hasn't changed (common during idle periods).
    last_state: Option<PoolState>,
}

impl PoolDoc {
    /// Create a new `PoolDoc` for the **daemon** with schema scaffolded.
    ///
    /// Sets a deterministic actor ID (`"runtimed:pool"`) and scaffolds the
    /// full schema so that all keys exist before the first sync round.
    /// Clients must use [`Self::new_empty()`] instead to avoid
    /// `DuplicateSeqNumber` conflicts.
    #[allow(clippy::expect_used, clippy::new_without_default)]
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(b"runtimed:pool" as &[u8]));

        // uv/
        let uv = doc
            .put_object(&ROOT, "uv", ObjType::Map)
            .expect("scaffold uv");
        doc.put(&uv, "available", 0u64)
            .expect("scaffold uv.available");
        doc.put(&uv, "warming", 0u64).expect("scaffold uv.warming");
        doc.put(&uv, "pool_size", 0u64)
            .expect("scaffold uv.pool_size");
        doc.put(&uv, "consecutive_failures", 0u64)
            .expect("scaffold uv.consecutive_failures");
        doc.put(&uv, "retry_in_secs", 0u64)
            .expect("scaffold uv.retry_in_secs");
        // error and failed_package are optional — only set when present

        // conda/
        let conda = doc
            .put_object(&ROOT, "conda", ObjType::Map)
            .expect("scaffold conda");
        doc.put(&conda, "available", 0u64)
            .expect("scaffold conda.available");
        doc.put(&conda, "warming", 0u64)
            .expect("scaffold conda.warming");
        doc.put(&conda, "pool_size", 0u64)
            .expect("scaffold conda.pool_size");
        doc.put(&conda, "consecutive_failures", 0u64)
            .expect("scaffold conda.consecutive_failures");
        doc.put(&conda, "retry_in_secs", 0u64)
            .expect("scaffold conda.retry_in_secs");

        // pixi/
        let pixi = doc
            .put_object(&ROOT, "pixi", ObjType::Map)
            .expect("scaffold pixi");
        doc.put(&pixi, "available", 0u64)
            .expect("scaffold pixi.available");
        doc.put(&pixi, "warming", 0u64)
            .expect("scaffold pixi.warming");
        doc.put(&pixi, "pool_size", 0u64)
            .expect("scaffold pixi.pool_size");
        doc.put(&pixi, "consecutive_failures", 0u64)
            .expect("scaffold pixi.consecutive_failures");
        doc.put(&pixi, "retry_in_secs", 0u64)
            .expect("scaffold pixi.retry_in_secs");

        Self {
            doc,
            last_state: None,
        }
    }

    /// Create an empty `PoolDoc` for read-only clients.
    ///
    /// The document starts empty with a random actor ID. All state
    /// arrives via Automerge sync from the daemon.
    pub fn new_empty() -> Self {
        Self {
            doc: AutoCommit::new(),
            last_state: None,
        }
    }

    /// Access the underlying Automerge document (read-only).
    pub fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// Access the underlying Automerge document (mutable, for sync protocol).
    pub fn doc_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }

    // ── Write ───────────────────────────────────────────────────────

    /// Update the pool state. Returns `true` if the document was mutated.
    ///
    /// Deduplicates writes — if the state hasn't changed since the last
    /// call, no Automerge operations are produced.
    pub fn update(&mut self, state: &PoolState) -> bool {
        if self.last_state.as_ref() == Some(state) {
            return false;
        }
        self.last_state = Some(state.clone());
        // write_state only fails on a corrupted doc, which can't happen
        // with the scaffolded schema.
        self.write_state(state).is_ok()
    }

    fn write_state(&mut self, state: &PoolState) -> Result<(), AutomergeError> {
        self.write_runtime_state("uv", &state.uv)?;
        self.write_runtime_state("conda", &state.conda)?;
        self.write_runtime_state("pixi", &state.pixi)?;
        Ok(())
    }

    fn write_runtime_state(
        &mut self,
        key: &str,
        state: &RuntimePoolState,
    ) -> Result<(), AutomergeError> {
        let (_, obj) = self
            .doc
            .get(&ROOT, key)?
            .ok_or_else(|| AutomergeError::InvalidObjId(format!("missing {key}")))?;
        self.doc.put(&obj, "available", state.available)?;
        self.doc.put(&obj, "warming", state.warming)?;
        self.doc.put(&obj, "pool_size", state.pool_size)?;
        self.doc.put(
            &obj,
            "consecutive_failures",
            state.consecutive_failures as u64,
        )?;
        self.doc.put(&obj, "retry_in_secs", state.retry_in_secs)?;

        // Error: set string when Some, delete key when None
        match &state.error {
            Some(msg) => {
                self.doc.put(&obj, "error", msg.as_str())?;
            }
            None => {
                let _ = self.doc.delete(&obj, "error");
            }
        }

        // Failed package: set string when Some, delete key when None
        match &state.failed_package {
            Some(pkg) => {
                self.doc.put(&obj, "failed_package", pkg.as_str())?;
            }
            None => {
                let _ = self.doc.delete(&obj, "failed_package");
            }
        }

        Ok(())
    }

    // ── Read ────────────────────────────────────────────────────────

    /// Read the full pool state snapshot from the document.
    pub fn read_state(&self) -> PoolState {
        PoolState {
            uv: self.read_runtime_state("uv"),
            conda: self.read_runtime_state("conda"),
            pixi: self.read_runtime_state("pixi"),
        }
    }

    fn read_runtime_state(&self, key: &str) -> RuntimePoolState {
        let Some((_, obj)) = self.doc.get(&ROOT, key).ok().flatten() else {
            return RuntimePoolState::default();
        };

        let get_u64 = |field: &str| -> u64 {
            self.doc
                .get(&obj, field)
                .ok()
                .flatten()
                .and_then(|(v, _)| match v {
                    Value::Scalar(s) => s.to_u64(),
                    _ => None,
                })
                .unwrap_or(0)
        };

        let get_str = |field: &str| -> Option<String> {
            self.doc
                .get(&obj, field)
                .ok()
                .flatten()
                .and_then(|(v, _)| match v {
                    Value::Scalar(s) => s.to_str().map(|s| s.to_string()),
                    _ => None,
                })
        };

        RuntimePoolState {
            available: get_u64("available"),
            warming: get_u64("warming"),
            pool_size: get_u64("pool_size"),
            error: get_str("error"),
            failed_package: get_str("failed_package"),
            consecutive_failures: get_u64("consecutive_failures") as u32,
            retry_in_secs: get_u64("retry_in_secs"),
        }
    }

    // ── Sync protocol ───────────────────────────────────────────────

    /// Generate a sync message to send to a peer.
    pub fn generate_sync_message(&mut self, peer_state: &mut sync::State) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(peer_state)
    }

    /// Receive a sync message from a client.
    ///
    /// **Read-only enforcement:** strips all `changes` from the client
    /// message. Preserves `heads`, `need`, `have` for the sync protocol
    /// handshake (bloom filter exchange, ACKs). The daemon is the sole
    /// writer — client mutations are discarded.
    pub fn receive_sync_message(
        &mut self,
        peer_state: &mut sync::State,
        mut message: sync::Message,
    ) -> Result<(), AutomergeError> {
        // Strip client changes — daemon is authoritative
        message.changes = Vec::<Vec<u8>>::new().into();
        self.doc
            .sync()
            .receive_sync_message(peer_state, message)
            .map(|_| ())
    }

    /// Round-trip save→load to rebuild internal automerge indices.
    pub fn rebuild_from_save(&mut self) -> bool {
        let actor = self.doc.get_actor().clone();
        let bytes = self.doc.save();
        match AutoCommit::load(&bytes) {
            Ok(mut doc) => {
                doc.set_actor(actor);
                self.doc = doc;
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_scaffolds_schema() {
        let doc = PoolDoc::new();
        let state = doc.read_state();
        assert_eq!(state.uv.available, 0);
        assert_eq!(state.uv.warming, 0);
        assert_eq!(state.uv.pool_size, 0);
        assert_eq!(state.uv.error, None);
        assert_eq!(state.conda.available, 0);
    }

    #[test]
    fn test_update_writes_state() {
        let mut doc = PoolDoc::new();
        let state = PoolState {
            uv: RuntimePoolState {
                available: 3,
                warming: 1,
                pool_size: 4,
                error: None,
                failed_package: None,
                consecutive_failures: 0,
                retry_in_secs: 0,
            },
            conda: RuntimePoolState {
                available: 2,
                warming: 0,
                pool_size: 3,
                error: Some("Failed to create env".into()),
                failed_package: Some("badpkg".into()),
                consecutive_failures: 2,
                retry_in_secs: 30,
            },
            pixi: RuntimePoolState::default(),
        };
        assert!(doc.update(&state));
        let read = doc.read_state();
        assert_eq!(read, state);
    }

    #[test]
    fn test_update_deduplicates() {
        let mut doc = PoolDoc::new();
        let state = PoolState {
            uv: RuntimePoolState {
                available: 3,
                warming: 1,
                pool_size: 4,
                ..Default::default()
            },
            conda: RuntimePoolState::default(),
            pixi: RuntimePoolState::default(),
        };
        assert!(doc.update(&state));
        assert!(!doc.update(&state)); // No change
    }

    #[test]
    fn test_sync_roundtrip() {
        let mut daemon_doc = PoolDoc::new();
        let state = PoolState {
            uv: RuntimePoolState {
                available: 2,
                warming: 1,
                pool_size: 4,
                error: Some("test error".into()),
                failed_package: Some("badpkg".into()),
                consecutive_failures: 3,
                retry_in_secs: 60,
            },
            conda: RuntimePoolState::default(),
            pixi: RuntimePoolState::default(),
        };
        daemon_doc.update(&state);

        // Client creates empty doc and syncs
        let mut client_doc = PoolDoc::new_empty();
        let mut daemon_peer = sync::State::new();
        let mut client_peer = sync::State::new();

        // Sync loop
        for _ in 0..10 {
            if let Some(msg) = daemon_doc.generate_sync_message(&mut daemon_peer) {
                client_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut client_peer, msg)
                    .unwrap();
            }
            if let Some(msg) = client_doc
                .doc_mut()
                .sync()
                .generate_sync_message(&mut client_peer)
            {
                daemon_doc
                    .receive_sync_message(&mut daemon_peer, msg)
                    .unwrap();
            }
        }

        let client_state = client_doc.read_state();
        assert_eq!(client_state, state);
    }

    #[test]
    fn test_read_only_enforcement() {
        let mut daemon_doc = PoolDoc::new();
        let mut client_doc = PoolDoc::new_empty();
        let mut daemon_peer = sync::State::new();
        let mut client_peer = sync::State::new();

        // Initial sync
        for _ in 0..10 {
            if let Some(msg) = daemon_doc.generate_sync_message(&mut daemon_peer) {
                client_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut client_peer, msg)
                    .unwrap();
            }
            if let Some(msg) = client_doc
                .doc_mut()
                .sync()
                .generate_sync_message(&mut client_peer)
            {
                daemon_doc
                    .receive_sync_message(&mut daemon_peer, msg)
                    .unwrap();
            }
        }

        // Client tries to write — should be stripped by daemon
        let (_, uv_id) = client_doc.doc_mut().get(&ROOT, "uv").unwrap().unwrap();
        client_doc
            .doc_mut()
            .put(&uv_id, "available", 999u64)
            .unwrap();

        // Sync client changes to daemon (should be stripped)
        for _ in 0..10 {
            if let Some(msg) = client_doc
                .doc_mut()
                .sync()
                .generate_sync_message(&mut client_peer)
            {
                daemon_doc
                    .receive_sync_message(&mut daemon_peer, msg)
                    .unwrap();
            }
            if let Some(msg) = daemon_doc.generate_sync_message(&mut daemon_peer) {
                client_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut client_peer, msg)
                    .unwrap();
            }
        }

        // Daemon state should be unchanged
        assert_eq!(daemon_doc.read_state().uv.available, 0);
    }
}
