//! PoolDoc — Automerge document for daemon pool state.
//!
//! A global, daemon-authoritative, ephemeral Automerge document that carries
//! the prewarmed environment pool state. Clients sync it read-only via the
//! standard Automerge sync protocol (same pattern as `SettingsDoc`).
//!
//! Schema:
//! ```text
//! ROOT/
//!   uv/
//!     available: u64
//!     warming: u64
//!     pool_size: u64
//!     error: Str | null
//!   conda/
//!     available: u64
//!     warming: u64
//!     pool_size: u64
//!     error: Str | null
//! ```
//!
//! The daemon writes to this doc on every warming loop tick and on every
//! error/recovery event. Clients receive updates via Automerge sync —
//! no dedicated broadcast subscription channel needed.

use automerge::{
    sync, sync::SyncDoc, transaction::Transactable, AutoCommit, AutomergeError, ObjType, ReadDoc,
    ROOT,
};
use log::warn;

/// Snapshot of pool state for a single runtime (uv or conda).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePoolState {
    pub available: u64,
    pub warming: u64,
    pub pool_size: u64,
    pub error: Option<String>,
}

/// Full pool state snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolState {
    pub uv: RuntimePoolState,
    pub conda: RuntimePoolState,
}

/// Automerge-backed pool state document.
///
/// The daemon creates one of these on startup. It's never persisted to disk —
/// pool state is ephemeral and reconstructed from the actual pool on restart.
pub struct PoolDoc {
    doc: AutoCommit,
    /// Cached last-written state to avoid redundant Automerge writes
    /// when the pool state hasn't changed (common during idle periods).
    last_state: Option<PoolState>,
}

impl PoolDoc {
    /// Create a new empty PoolDoc with the schema scaffolded.
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(b"runtimed:pool".as_slice()));

        // Scaffold the schema
        let uv = doc
            .put_object(ROOT, "uv", ObjType::Map)
            .expect("scaffold uv");
        doc.put(&uv, "available", 0u64)
            .expect("scaffold uv.available");
        doc.put(&uv, "warming", 0u64).expect("scaffold uv.warming");
        doc.put(&uv, "pool_size", 0u64)
            .expect("scaffold uv.pool_size");
        // error starts as null (absent)

        let conda = doc
            .put_object(ROOT, "conda", ObjType::Map)
            .expect("scaffold conda");
        doc.put(&conda, "available", 0u64)
            .expect("scaffold conda.available");
        doc.put(&conda, "warming", 0u64)
            .expect("scaffold conda.warming");
        doc.put(&conda, "pool_size", 0u64)
            .expect("scaffold conda.pool_size");

        PoolDoc {
            doc,
            last_state: None,
        }
    }

    /// Write the full pool state to the document.
    ///
    /// Returns `true` if the doc was actually mutated (state changed).
    /// Returns `false` if the state is identical to the last write (no-op).
    ///
    /// This is called on every warming loop tick (~30s) so the dedup check
    /// avoids growing Automerge history during idle periods.
    pub fn update(&mut self, state: &PoolState) -> bool {
        // Fast path: skip if unchanged
        if self.last_state.as_ref() == Some(state) {
            return false;
        }

        if let Err(e) = self.write_state(state) {
            warn!("[pool-doc] Failed to write pool state: {}", e);
            return false;
        }

        self.last_state = Some(state.clone());
        true
    }

    /// Internal: write the state fields into the Automerge doc.
    fn write_state(&mut self, state: &PoolState) -> Result<(), AutomergeError> {
        // Write uv/ fields
        let uv_id = self
            .doc
            .get(ROOT, "uv")?
            .map(|(_, id)| id)
            .ok_or(AutomergeError::InvalidIndex(0))?;

        self.doc.put(&uv_id, "available", state.uv.available)?;
        self.doc.put(&uv_id, "warming", state.uv.warming)?;
        self.doc.put(&uv_id, "pool_size", state.uv.pool_size)?;
        match &state.uv.error {
            Some(err) => self.doc.put(&uv_id, "error", err.as_str())?,
            None => {
                // Delete the key to represent null. If the key doesn't exist,
                // delete is a no-op — safe to call unconditionally.
                let _ = self.doc.delete(&uv_id, "error");
            }
        }

        // Write conda/ fields
        let conda_id = self
            .doc
            .get(ROOT, "conda")?
            .map(|(_, id)| id)
            .ok_or(AutomergeError::InvalidIndex(0))?;

        self.doc
            .put(&conda_id, "available", state.conda.available)?;
        self.doc.put(&conda_id, "warming", state.conda.warming)?;
        self.doc
            .put(&conda_id, "pool_size", state.conda.pool_size)?;
        match &state.conda.error {
            Some(err) => self.doc.put(&conda_id, "error", err.as_str())?,
            None => {
                let _ = self.doc.delete(&conda_id, "error");
            }
        }

        Ok(())
    }

    /// Read the current pool state from the document.
    ///
    /// Used by the frontend-facing sync handler to verify state, and by tests.
    pub fn read_state(&self) -> Option<PoolState> {
        let uv = self.read_runtime_state("uv")?;
        let conda = self.read_runtime_state("conda")?;
        Some(PoolState { uv, conda })
    }

    fn read_runtime_state(&self, key: &str) -> Option<RuntimePoolState> {
        let (_, obj_id) = self.doc.get(ROOT, key).ok()??;

        let available = self
            .doc
            .get(&obj_id, "available")
            .ok()?
            .and_then(|(v, _)| v.to_u64())
            .unwrap_or(0);

        let warming = self
            .doc
            .get(&obj_id, "warming")
            .ok()?
            .and_then(|(v, _)| v.to_u64())
            .unwrap_or(0);

        let pool_size = self
            .doc
            .get(&obj_id, "pool_size")
            .ok()?
            .and_then(|(v, _)| v.to_u64())
            .unwrap_or(0);

        let error = self
            .doc
            .get(&obj_id, "error")
            .ok()
            .flatten()
            .and_then(|(v, _)| v.to_str().map(|s| s.to_string()));

        Some(RuntimePoolState {
            available,
            warming,
            pool_size,
            error,
        })
    }

    // ── Automerge sync protocol ──────────────────────────────────────

    /// Generate a sync message for a peer.
    pub fn generate_sync_message(&mut self, peer_state: &mut sync::State) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(peer_state)
    }

    /// Receive a sync message from a peer.
    ///
    /// For PoolDoc this is used to complete the sync protocol handshake
    /// (the client sends back acknowledgments). We intentionally strip
    /// any document changes from the client — the pool doc is
    /// daemon-authoritative and read-only for clients.
    pub fn receive_sync_message(
        &mut self,
        peer_state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        // Strip any changes the client may have included — the daemon is the
        // sole authority for pool state. We preserve heads/need/have so the
        // sync protocol handshake (bloom filter exchange, ACKs) still works.
        let filtered = sync::Message {
            heads: message.heads,
            need: message.need,
            have: message.have,
            changes: sync::ChunkList::empty(),
            supported_capabilities: message.supported_capabilities,
            version: message.version,
        };
        self.doc.sync().receive_sync_message(peer_state, filtered)?;
        Ok(())
    }
}

impl Default for PoolDoc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_doc_has_zero_state() {
        let doc = PoolDoc::new();
        let state = doc.read_state().unwrap();
        assert_eq!(state.uv.available, 0);
        assert_eq!(state.uv.warming, 0);
        assert_eq!(state.uv.pool_size, 0);
        assert_eq!(state.uv.error, None);
        assert_eq!(state.conda.available, 0);
        assert_eq!(state.conda.warming, 0);
        assert_eq!(state.conda.pool_size, 0);
        assert_eq!(state.conda.error, None);
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
            },
            conda: RuntimePoolState {
                available: 2,
                warming: 0,
                pool_size: 3,
                error: Some("rattler failed".to_string()),
            },
        };

        let changed = doc.update(&state);
        assert!(changed);

        let read = doc.read_state().unwrap();
        assert_eq!(read.uv.available, 3);
        assert_eq!(read.uv.warming, 1);
        assert_eq!(read.uv.pool_size, 4);
        assert_eq!(read.uv.error, None);
        assert_eq!(read.conda.available, 2);
        assert_eq!(read.conda.warming, 0);
        assert_eq!(read.conda.pool_size, 3);
        assert_eq!(read.conda.error, Some("rattler failed".to_string()));
    }

    #[test]
    fn test_update_deduplicates() {
        let mut doc = PoolDoc::new();
        let state = PoolState {
            uv: RuntimePoolState {
                available: 3,
                warming: 0,
                pool_size: 3,
                error: None,
            },
            conda: RuntimePoolState {
                available: 0,
                warming: 0,
                pool_size: 0,
                error: None,
            },
        };

        assert!(doc.update(&state)); // first write — changed
        assert!(!doc.update(&state)); // same state — no-op
    }

    #[test]
    fn test_error_cleared() {
        let mut doc = PoolDoc::new();

        // Set an error
        let with_error = PoolState {
            uv: RuntimePoolState {
                available: 0,
                warming: 0,
                pool_size: 2,
                error: Some("venv creation failed".to_string()),
            },
            conda: RuntimePoolState {
                available: 0,
                warming: 0,
                pool_size: 0,
                error: None,
            },
        };
        doc.update(&with_error);
        assert_eq!(
            doc.read_state().unwrap().uv.error,
            Some("venv creation failed".to_string())
        );

        // Clear the error
        let without_error = PoolState {
            uv: RuntimePoolState {
                available: 2,
                warming: 0,
                pool_size: 2,
                error: None,
            },
            conda: RuntimePoolState {
                available: 0,
                warming: 0,
                pool_size: 0,
                error: None,
            },
        };
        doc.update(&without_error);
        assert_eq!(doc.read_state().unwrap().uv.error, None);
    }

    #[test]
    fn test_sync_between_two_docs() {
        let mut server = PoolDoc::new();
        let mut client_doc = AutoCommit::new();
        let mut server_to_client = sync::State::new();
        let mut client_to_server = sync::State::new();

        // Server writes some state
        let state = PoolState {
            uv: RuntimePoolState {
                available: 4,
                warming: 0,
                pool_size: 4,
                error: None,
            },
            conda: RuntimePoolState {
                available: 1,
                warming: 2,
                pool_size: 3,
                error: None,
            },
        };
        server.update(&state);

        // Sync: server → client
        for _ in 0..10 {
            let msg_s = server.generate_sync_message(&mut server_to_client);
            let msg_c = client_doc
                .sync()
                .generate_sync_message(&mut client_to_server);

            if msg_s.is_none() && msg_c.is_none() {
                break;
            }
            if let Some(msg) = msg_s {
                client_doc
                    .sync()
                    .receive_sync_message(&mut client_to_server, msg)
                    .unwrap();
            }
            if let Some(msg) = msg_c {
                server
                    .receive_sync_message(&mut server_to_client, msg)
                    .unwrap();
            }
        }

        // Client should have the same state
        let (_, uv_id) = client_doc.get(ROOT, "uv").unwrap().unwrap();
        let (avail, _) = client_doc.get(&uv_id, "available").unwrap().unwrap();
        assert_eq!(avail.to_u64(), Some(4));

        let (_, conda_id) = client_doc.get(ROOT, "conda").unwrap().unwrap();
        let (warming, _) = client_doc.get(&conda_id, "warming").unwrap().unwrap();
        assert_eq!(warming.to_u64(), Some(2));
    }
}
