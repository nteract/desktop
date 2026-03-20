//! RuntimeStateDoc — per-notebook ephemeral Automerge document for runtime state.
//!
//! Daemon-authoritative. One per notebook room. Describes the kernel, execution
//! queue, and environment state. Clients sync read-only via the Automerge sync
//! protocol — the daemon strips any client-side changes.
//!
//! Schema:
//! ```text
//! ROOT/
//!   kernel/
//!     status: Str          ("idle" | "busy" | "starting" | "error" | "shutdown" | "not_started")
//!     name: Str            (e.g. "charming-toucan")
//!     language: Str        (e.g. "python", "typescript")
//!     env_source: Str      (e.g. "uv:prewarmed", "conda:pixi", "deno")
//!   queue/
//!     executing: Str|null  (cell_id currently executing)
//!     queued: List[Str]    (cell_ids waiting)
//!   env/
//!     in_sync: bool
//!     added: List[Str]     (packages in metadata but not in kernel)
//!     removed: List[Str]   (packages in kernel but not in metadata)
//!     channels_changed: bool
//!     deno_changed: bool
//!   last_saved: Str|null   (ISO timestamp of last save)
//! ```

use automerge::{
    sync, sync::SyncDoc, transaction::Transactable, ActorId, AutoCommit, AutomergeError, ObjType,
    ReadDoc, ScalarValue, Value, ROOT,
};
use serde::{Deserialize, Serialize};

// ── Snapshot types for reading/comparing state ──────────────────────

/// Kernel state snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelState {
    pub status: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub env_source: String,
}

impl Default for KernelState {
    fn default() -> Self {
        Self {
            status: "not_started".to_string(),
            name: String::new(),
            language: String::new(),
            env_source: String::new(),
        }
    }
}

/// Queue state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    pub executing: Option<String>,
    pub queued: Vec<String>,
}

/// Environment sync state snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvState {
    pub in_sync: bool,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub channels_changed: bool,
    #[serde(default)]
    pub deno_changed: bool,
}

impl Default for EnvState {
    fn default() -> Self {
        Self {
            in_sync: true,
            added: Vec::new(),
            removed: Vec::new(),
            channels_changed: false,
            deno_changed: false,
        }
    }
}

/// Full runtime state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    pub kernel: KernelState,
    pub queue: QueueState,
    pub env: EnvState,
    pub last_saved: Option<String>,
}

// ── RuntimeStateDoc ─────────────────────────────────────────────────

/// Per-notebook ephemeral Automerge document for runtime state.
///
/// The daemon creates one of these per notebook room. Clients receive
/// updates via Automerge sync — never by direct mutation.
pub struct RuntimeStateDoc {
    doc: AutoCommit,
}

impl RuntimeStateDoc {
    /// Create a new `RuntimeStateDoc` for the **daemon** with schema scaffolded.
    ///
    /// Sets a deterministic actor ID (`"runtimed:state"`) and scaffolds the
    /// full schema so that all keys exist before the first sync round.
    /// Clients must use [`Self::new_empty()`] instead to avoid
    /// `DuplicateSeqNumber` conflicts from a second doc with the same actor.
    #[allow(clippy::expect_used, clippy::new_without_default)]
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(b"runtimed:state" as &[u8]));

        // kernel/
        let kernel = doc
            .put_object(&ROOT, "kernel", ObjType::Map)
            .expect("scaffold kernel");
        doc.put(&kernel, "status", "not_started")
            .expect("scaffold kernel.status");
        doc.put(&kernel, "name", "").expect("scaffold kernel.name");
        doc.put(&kernel, "language", "")
            .expect("scaffold kernel.language");
        doc.put(&kernel, "env_source", "")
            .expect("scaffold kernel.env_source");

        // queue/
        let queue = doc
            .put_object(&ROOT, "queue", ObjType::Map)
            .expect("scaffold queue");
        doc.put(&queue, "executing", ScalarValue::Null)
            .expect("scaffold queue.executing");
        doc.put_object(&queue, "queued", ObjType::List)
            .expect("scaffold queue.queued");

        // env/
        let env = doc
            .put_object(&ROOT, "env", ObjType::Map)
            .expect("scaffold env");
        doc.put(&env, "in_sync", true)
            .expect("scaffold env.in_sync");
        doc.put_object(&env, "added", ObjType::List)
            .expect("scaffold env.added");
        doc.put_object(&env, "removed", ObjType::List)
            .expect("scaffold env.removed");
        doc.put(&env, "channels_changed", false)
            .expect("scaffold env.channels_changed");
        doc.put(&env, "deno_changed", false)
            .expect("scaffold env.deno_changed");

        // last_saved
        doc.put(&ROOT, "last_saved", ScalarValue::Null)
            .expect("scaffold last_saved");

        Self { doc }
    }

    /// Create an empty `RuntimeStateDoc` for read-only clients.
    ///
    /// The document starts empty with a random actor ID. All state
    /// arrives via Automerge sync from the daemon. This avoids
    /// `DuplicateSeqNumber` conflicts that would occur if the client
    /// scaffolded the same schema with the same actor ID as the daemon.
    pub fn new_empty() -> Self {
        Self {
            doc: AutoCommit::new(),
            // No scaffolding — sync populates everything
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

    // ── Helpers ─────────────────────────────────────────────────────

    /// Get the ObjId for a top-level map key.
    fn get_map(&self, key: &str) -> Option<automerge::ObjId> {
        self.doc
            .get(&ROOT, key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    /// Read a string scalar from a map object.
    fn read_str(&self, obj: &automerge::ObjId, key: &str) -> String {
        self.doc
            .get(obj, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Read an optional string (null → None) from a map object.
    fn read_opt_str(&self, obj: &automerge::ObjId, key: &str) -> Option<String> {
        self.doc
            .get(obj, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Null => None,
                    ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            })
    }

    /// Read a bool scalar from a map object.
    fn read_bool(&self, obj: &automerge::ObjId, key: &str) -> bool {
        self.doc
            .get(obj, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Boolean(b) => Some(*b),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or(false)
    }

    /// Read a List[Str] from a map object.
    fn read_str_list(&self, obj: &automerge::ObjId, key: &str) -> Vec<String> {
        let Some(list_id) =
            self.doc
                .get(obj, key)
                .ok()
                .flatten()
                .and_then(|(value, id)| match value {
                    Value::Object(ObjType::List) => Some(id),
                    _ => None,
                })
        else {
            return Vec::new();
        };
        let len = self.doc.length(&list_id);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let Some(s) = self
                .doc
                .get(&list_id, i)
                .ok()
                .flatten()
                .and_then(|(value, _)| match value {
                    Value::Scalar(s) => match s.as_ref() {
                        ScalarValue::Str(s) => Some(s.to_string()),
                        _ => None,
                    },
                    _ => None,
                })
            {
                out.push(s);
            }
        }
        out
    }

    // ── Granular setters (daemon calls these individually) ──────────

    /// Update kernel status. Returns `true` if the doc was mutated.
    #[allow(clippy::expect_used)]
    pub fn set_kernel_status(&mut self, status: &str) -> bool {
        let kernel = self.get_map("kernel").expect("kernel map must exist");
        let current = self.read_str(&kernel, "status");
        if current == status {
            return false;
        }
        self.doc
            .put(&kernel, "status", status)
            .expect("put kernel.status");
        true
    }

    /// Update kernel info (name, language, env_source). Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_kernel_info(&mut self, name: &str, language: &str, env_source: &str) -> bool {
        let kernel = self.get_map("kernel").expect("kernel map must exist");
        let cur_name = self.read_str(&kernel, "name");
        let cur_lang = self.read_str(&kernel, "language");
        let cur_src = self.read_str(&kernel, "env_source");
        if cur_name == name && cur_lang == language && cur_src == env_source {
            return false;
        }
        self.doc
            .put(&kernel, "name", name)
            .expect("put kernel.name");
        self.doc
            .put(&kernel, "language", language)
            .expect("put kernel.language");
        self.doc
            .put(&kernel, "env_source", env_source)
            .expect("put kernel.env_source");
        true
    }

    /// Update queue state. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_queue(&mut self, executing: Option<&str>, queued: &[String]) -> bool {
        let queue = self.get_map("queue").expect("queue map must exist");
        let cur_executing = self.read_opt_str(&queue, "executing");
        let cur_queued = self.read_str_list(&queue, "queued");

        let exec_match = match (&cur_executing, executing) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };

        if exec_match && cur_queued.len() == queued.len() && cur_queued == queued {
            return false;
        }

        match executing {
            Some(cell_id) => self
                .doc
                .put(&queue, "executing", cell_id)
                .expect("put queue.executing"),
            None => self
                .doc
                .put(&queue, "executing", ScalarValue::Null)
                .expect("put queue.executing null"),
        }

        // Reuse the existing list object to avoid Automerge object churn.
        let list = self
            .doc
            .get(&queue, "queued")
            .expect("get queue.queued")
            .map(|(_, id)| id)
            .expect("queued list must exist");
        // Clear existing entries (reverse order for stable indices)
        let len = self.doc.length(&list);
        for i in (0..len).rev() {
            self.doc.delete(&list, i).expect("delete queued entry");
        }
        // Insert new entries
        for (i, cell_id) in queued.iter().enumerate() {
            self.doc
                .insert(&list, i, cell_id.as_str())
                .expect("insert queued cell_id");
        }

        true
    }

    /// Update environment sync state. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_env_sync(
        &mut self,
        in_sync: bool,
        added: &[String],
        removed: &[String],
        channels_changed: bool,
        deno_changed: bool,
    ) -> bool {
        let env = self.get_map("env").expect("env map must exist");
        let cur_in_sync = self.read_bool(&env, "in_sync");
        let cur_added = self.read_str_list(&env, "added");
        let cur_removed = self.read_str_list(&env, "removed");
        let cur_channels = self.read_bool(&env, "channels_changed");
        let cur_deno = self.read_bool(&env, "deno_changed");

        if cur_in_sync == in_sync
            && cur_added == added
            && cur_removed == removed
            && cur_channels == channels_changed
            && cur_deno == deno_changed
        {
            return false;
        }

        self.doc
            .put(&env, "in_sync", in_sync)
            .expect("put env.in_sync");
        self.doc
            .put(&env, "channels_changed", channels_changed)
            .expect("put env.channels_changed");
        self.doc
            .put(&env, "deno_changed", deno_changed)
            .expect("put env.deno_changed");

        // Reuse existing list objects to avoid Automerge object churn.
        let added_list = self
            .doc
            .get(&env, "added")
            .expect("get env.added")
            .map(|(_, id)| id)
            .expect("added list must exist");
        let len = self.doc.length(&added_list);
        for i in (0..len).rev() {
            self.doc.delete(&added_list, i).expect("delete added entry");
        }
        for (i, pkg) in added.iter().enumerate() {
            self.doc
                .insert(&added_list, i, pkg.as_str())
                .expect("insert added pkg");
        }

        let removed_list = self
            .doc
            .get(&env, "removed")
            .expect("get env.removed")
            .map(|(_, id)| id)
            .expect("removed list must exist");
        let len = self.doc.length(&removed_list);
        for i in (0..len).rev() {
            self.doc
                .delete(&removed_list, i)
                .expect("delete removed entry");
        }
        for (i, pkg) in removed.iter().enumerate() {
            self.doc
                .insert(&removed_list, i, pkg.as_str())
                .expect("insert removed pkg");
        }

        true
    }

    /// Set the `last_saved` timestamp. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_last_saved(&mut self, timestamp: Option<&str>) -> bool {
        let current = self
            .doc
            .get(&ROOT, "last_saved")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Null => None,
                    ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            });

        let matches = match (&current, timestamp) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };

        if matches {
            return false;
        }

        match timestamp {
            Some(ts) => self
                .doc
                .put(&ROOT, "last_saved", ts)
                .expect("put last_saved"),
            None => self
                .doc
                .put(&ROOT, "last_saved", ScalarValue::Null)
                .expect("put last_saved null"),
        }

        true
    }

    // ── Full state read ─────────────────────────────────────────────

    /// Read the full runtime state snapshot.
    pub fn read_state(&self) -> RuntimeState {
        let kernel = self.get_map("kernel");
        let queue = self.get_map("queue");
        let env = self.get_map("env");

        let kernel_state = kernel
            .as_ref()
            .map(|k| KernelState {
                status: self.read_str(k, "status"),
                name: self.read_str(k, "name"),
                language: self.read_str(k, "language"),
                env_source: self.read_str(k, "env_source"),
            })
            .unwrap_or_default();

        let queue_state = queue
            .as_ref()
            .map(|q| QueueState {
                executing: self.read_opt_str(q, "executing"),
                queued: self.read_str_list(q, "queued"),
            })
            .unwrap_or_default();

        let env_state = env
            .as_ref()
            .map(|e| EnvState {
                in_sync: self.read_bool(e, "in_sync"),
                added: self.read_str_list(e, "added"),
                removed: self.read_str_list(e, "removed"),
                channels_changed: self.read_bool(e, "channels_changed"),
                deno_changed: self.read_bool(e, "deno_changed"),
            })
            .unwrap_or_default();

        let last_saved = self
            .doc
            .get(&ROOT, "last_saved")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Null => None,
                    ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            });

        RuntimeState {
            kernel: kernel_state,
            queue: queue_state,
            env: env_state,
            last_saved,
        }
    }

    // ── Automerge sync protocol ─────────────────────────────────────

    /// Generate an outbound sync message for a peer.
    pub fn generate_sync_message(&mut self, peer_state: &mut sync::State) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(peer_state)
    }

    /// Receive a sync message with change stripping (read-only enforcement).
    ///
    /// The daemon is the sole authority for runtime state. Any changes a
    /// client embeds in its sync message are stripped — only the heads/need/have
    /// handshake is processed so the client can catch up.
    pub fn receive_sync_message(
        &mut self,
        peer_state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        // Strip client changes — keep only the sync protocol handshake.
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

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_doc_has_default_state() {
        let doc = RuntimeStateDoc::new();
        let state = doc.read_state();
        assert_eq!(state, RuntimeState::default());
        assert_eq!(state.kernel.status, "not_started");
        assert_eq!(state.kernel.name, "");
        assert_eq!(state.kernel.language, "");
        assert_eq!(state.kernel.env_source, "");
        assert!(state.queue.executing.is_none());
        assert!(state.queue.queued.is_empty());
        assert!(state.env.in_sync);
        assert!(state.env.added.is_empty());
        assert!(state.env.removed.is_empty());
        assert!(!state.env.channels_changed);
        assert!(!state.env.deno_changed);
        assert!(state.last_saved.is_none());
    }

    #[test]
    fn test_set_kernel_status() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.set_kernel_status("busy"));
        assert_eq!(doc.read_state().kernel.status, "busy");

        assert!(doc.set_kernel_status("idle"));
        assert_eq!(doc.read_state().kernel.status, "idle");
    }

    #[test]
    fn test_set_kernel_info() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.set_kernel_info("charming-toucan", "python", "uv:prewarmed"));
        let state = doc.read_state();
        assert_eq!(state.kernel.name, "charming-toucan");
        assert_eq!(state.kernel.language, "python");
        assert_eq!(state.kernel.env_source, "uv:prewarmed");
    }

    #[test]
    fn test_set_queue() {
        let mut doc = RuntimeStateDoc::new();
        let queued = vec!["cell-2".to_string(), "cell-3".to_string()];
        assert!(doc.set_queue(Some("cell-1"), &queued));

        let state = doc.read_state();
        assert_eq!(state.queue.executing, Some("cell-1".to_string()));
        assert_eq!(state.queue.queued, queued);
    }

    #[test]
    fn test_set_env_sync() {
        let mut doc = RuntimeStateDoc::new();
        let added = vec!["numpy".to_string(), "pandas".to_string()];
        let removed = vec!["scipy".to_string()];
        assert!(doc.set_env_sync(false, &added, &removed, true, false));

        let state = doc.read_state();
        assert!(!state.env.in_sync);
        assert_eq!(state.env.added, added);
        assert_eq!(state.env.removed, removed);
        assert!(state.env.channels_changed);
        assert!(!state.env.deno_changed);
    }

    #[test]
    fn test_set_last_saved() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.set_last_saved(Some("2025-01-15T12:00:00Z")));
        assert_eq!(
            doc.read_state().last_saved,
            Some("2025-01-15T12:00:00Z".to_string())
        );

        // Set to None
        assert!(doc.set_last_saved(None));
        assert_eq!(doc.read_state().last_saved, None);
    }

    #[test]
    fn test_dedup_skips_redundant_writes() {
        let mut doc = RuntimeStateDoc::new();

        // First write mutates
        assert!(doc.set_kernel_status("busy"));
        // Same value — no mutation
        assert!(!doc.set_kernel_status("busy"));

        // Same for kernel info
        assert!(doc.set_kernel_info("k", "python", "uv:prewarmed"));
        assert!(!doc.set_kernel_info("k", "python", "uv:prewarmed"));

        // Same for queue
        let q = vec!["a".to_string()];
        assert!(doc.set_queue(Some("x"), &q));
        assert!(!doc.set_queue(Some("x"), &q));

        // Same for env — defaults are (true, [], [], false, false),
        // so use non-default values to get an initial mutation.
        assert!(doc.set_env_sync(false, &[], &[], false, false));
        assert!(!doc.set_env_sync(false, &[], &[], false, false));

        // Same for last_saved
        assert!(doc.set_last_saved(Some("2025-01-15T12:00:00Z")));
        assert!(!doc.set_last_saved(Some("2025-01-15T12:00:00Z")));
    }

    #[test]
    fn test_sync_between_two_docs() {
        let mut daemon_doc = RuntimeStateDoc::new();
        daemon_doc.set_kernel_status("busy");
        daemon_doc.set_kernel_info("charming-toucan", "python", "uv:prewarmed");
        daemon_doc.set_queue(
            Some("cell-1"),
            &["cell-2".to_string(), "cell-3".to_string()],
        );
        daemon_doc.set_env_sync(
            false,
            &["numpy".to_string()],
            &["scipy".to_string()],
            true,
            false,
        );
        daemon_doc.set_last_saved(Some("2025-01-15T12:00:00Z"));

        // Client uses new_empty() — random actor, no scaffolding.
        let mut client_doc = RuntimeStateDoc::new_empty();
        let mut daemon_sync = sync::State::new();
        let mut client_sync = sync::State::new();

        // Run sync rounds until converged.
        for _ in 0..10 {
            if let Some(msg) = daemon_doc.generate_sync_message(&mut daemon_sync) {
                client_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut client_sync, msg)
                    .expect("client receive");
            }
            if let Some(msg) = client_doc
                .doc_mut()
                .sync()
                .generate_sync_message(&mut client_sync)
            {
                daemon_doc
                    .receive_sync_message(&mut daemon_sync, msg)
                    .expect("daemon receive");
            }
        }

        let daemon_state = daemon_doc.read_state();
        let client_state = client_doc.read_state();
        assert_eq!(daemon_state, client_state);
        assert_eq!(client_state.kernel.status, "busy");
        assert_eq!(client_state.kernel.name, "charming-toucan");
        assert_eq!(client_state.queue.executing, Some("cell-1".to_string()));
        assert_eq!(client_state.queue.queued.len(), 2);
        assert!(!client_state.env.in_sync);
        assert_eq!(
            client_state.last_saved,
            Some("2025-01-15T12:00:00Z".to_string()),
        );
    }
}
