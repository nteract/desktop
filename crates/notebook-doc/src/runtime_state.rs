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
//!     starting_phase: Str  ("" | "resolving" | "preparing_env" | "launching" | "connecting")
//!     name: Str            (e.g. "charming-toucan")
//!     language: Str        (e.g. "python", "typescript")
//!     env_source: Str      (e.g. "uv:prewarmed", "pixi:toml", "deno")
//!   queue/
//!     executing: Str|null              (cell_id currently executing)
//!     executing_execution_id: Str|null (execution_id for the executing cell)
//!     queued: List[Str]                (cell_ids waiting)
//!     queued_execution_ids: List[Str]  (parallel execution_ids for queued entries)
//!   executions/             Map (keyed by execution_id)
//!     {execution_id}/       Map
//!       cell_id: Str
//!       status: Str         ("queued" | "running" | "done" | "error")
//!       execution_count: Int|null
//!       success: Bool|null
//!       outputs: List[Map]  (inline manifests with blob refs)
//!         {output}/
//!           output_type: Str
//!           data: Map { mime_type: Map { blob: Str, size: Uint } }  (display_data/execute_result)
//!           metadata: Map (JSON)
//!           execution_count: Int|null (execute_result only)
//!           transient: Map { display_id: Str }
//!           name: Str (stream only)
//!           text: Map { blob: Str, size: Uint } (stream only)
//!           ename: Str (error only)
//!           evalue: Str (error only)
//!           traceback: Map { blob: Str, size: Uint } (error only)
//!   env/
//!     in_sync: bool
//!     added: List[Str]     (packages in metadata but not in kernel)
//!     removed: List[Str]   (packages in kernel but not in metadata)
//!     channels_changed: bool
//!     deno_changed: bool
//!   trust/
//!     status: Str          ("trusted" | "untrusted" | "signature_invalid" | "no_dependencies")
//!     needs_approval: bool
//!   comms/                 Map (keyed by comm_id)
//!     {comm_id}/           Map
//!       target_name: Str
//!       model_module: Str
//!       model_name: Str
//!       state: Str         (JSON-encoded widget state)
//!       outputs: List[Map] (inline manifests, OutputModel only)
//!       seq: Int           (insertion order)
//!       capture_msg_id: Str (Output widget capture routing, "" if not capturing)
//!   last_saved: Str|null   (ISO timestamp of last save)
//! ```

use automerge::{
    sync, sync::SyncDoc, transaction::Transactable, ActorId, AutoCommit, AutomergeError, ObjType,
    ReadDoc, ScalarValue, Value, ROOT,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-export StreamOutputState so callers can use it from either location.
pub use crate::StreamOutputState;

// ── Snapshot types for reading/comparing state ──────────────────────

/// Kernel state snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelState {
    pub status: String,
    #[serde(default)]
    pub starting_phase: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub env_source: String,
    /// ID of the runtime agent subprocess that owns this kernel (e.g., "runtime-agent:a1b2c3d4").
    /// Used for provenance — identifying which runtime agent is running and detecting stale ones.
    #[serde(default)]
    pub runtime_agent_id: String,
}

impl Default for KernelState {
    fn default() -> Self {
        Self {
            status: "not_started".to_string(),
            starting_phase: String::new(),
            name: String::new(),
            language: String::new(),
            env_source: String::new(),
            runtime_agent_id: String::new(),
        }
    }
}

/// An entry in the execution queue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub cell_id: String,
    pub execution_id: String,
}

/// Queue state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    pub executing: Option<QueueEntry>,
    pub queued: Vec<QueueEntry>,
}

/// Execution lifecycle state for a single execution.
///
/// Tracks the status of an execution from queue to completion.
/// Stored in `executions/{execution_id}/` in the RuntimeStateDoc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Cell that was executed.
    pub cell_id: String,
    /// Current status: "queued", "running", "done", "error".
    pub status: String,
    /// Kernel execution count (set when execution starts).
    #[serde(default)]
    pub execution_count: Option<i64>,
    /// Whether the execution succeeded (set on completion).
    #[serde(default)]
    pub success: Option<bool>,
    /// Output manifests for this execution (inline Automerge Maps with blob refs).
    #[serde(default)]
    pub outputs: Vec<serde_json::Value>,
    /// Source code that was executed (audit log).
    /// Set by the coordinator when creating the execution entry.
    #[serde(default)]
    pub source: Option<String>,
    /// Queue sequence number for ordering.
    /// Monotonic counter owned by the coordinator; the runtime agent sorts
    /// queued entries by this to determine execution order.
    #[serde(default)]
    pub seq: Option<u64>,
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
    /// Packages pre-installed in the prewarmed environment (empty for inline envs).
    #[serde(default)]
    pub prewarmed_packages: Vec<String>,
}

impl Default for EnvState {
    fn default() -> Self {
        Self {
            in_sync: true,
            added: Vec::new(),
            removed: Vec::new(),
            channels_changed: false,
            deno_changed: false,
            prewarmed_packages: Vec::new(),
        }
    }
}

/// Trust state snapshot for the runtime state doc.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRuntimeState {
    /// "trusted", "untrusted", "signature_invalid", "no_dependencies"
    pub status: String,
    /// Whether the frontend should show the trust approval dialog
    pub needs_approval: bool,
}

/// Snapshot of a single comm entry in the RuntimeStateDoc.
///
/// State is stored as a native Automerge map for per-property merge.
/// Two peers editing different widget properties (e.g., `value` and
/// `description`) compose cleanly via CRDT semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommDocEntry {
    pub target_name: String,
    #[serde(default)]
    pub model_module: String,
    #[serde(default)]
    pub model_name: String,
    /// Widget state as a JSON object (stored as native Automerge map).
    #[serde(default = "default_empty_state")]
    pub state: serde_json::Value,
    /// Output manifests (inline Automerge Maps, OutputModel widgets only).
    #[serde(default)]
    pub outputs: Vec<serde_json::Value>,
    /// Insertion order for dependency-correct replay.
    #[serde(default)]
    pub seq: u64,
    /// The msg_id this Output widget is capturing (empty = not capturing).
    /// When set, kernel outputs with matching parent_header.msg_id are routed
    /// to this widget instead of cell outputs.
    #[serde(default)]
    pub capture_msg_id: String,
}

fn default_empty_state() -> serde_json::Value {
    serde_json::json!({})
}

/// Full runtime state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeState {
    pub kernel: KernelState,
    pub queue: QueueState,
    pub env: EnvState,
    pub trust: TrustRuntimeState,
    pub last_saved: Option<String>,
    /// Execution lifecycle entries keyed by execution_id.
    #[serde(default)]
    pub executions: HashMap<String, ExecutionState>,
    /// Active comm channels keyed by comm_id.
    #[serde(default)]
    pub comms: HashMap<String, CommDocEntry>,
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
        doc.put(&kernel, "runtime_agent_id", "")
            .expect("scaffold kernel.runtime_agent_id");
        doc.put(&kernel, "starting_phase", "")
            .expect("scaffold kernel.starting_phase");

        // queue/
        let queue = doc
            .put_object(&ROOT, "queue", ObjType::Map)
            .expect("scaffold queue");
        doc.put(&queue, "executing", ScalarValue::Null)
            .expect("scaffold queue.executing");
        doc.put(&queue, "executing_execution_id", ScalarValue::Null)
            .expect("scaffold queue.executing_execution_id");
        doc.put_object(&queue, "queued", ObjType::List)
            .expect("scaffold queue.queued");
        doc.put_object(&queue, "queued_execution_ids", ObjType::List)
            .expect("scaffold queue.queued_execution_ids");

        // executions/ — keyed by execution_id, tracks lifecycle
        doc.put_object(&ROOT, "executions", ObjType::Map)
            .expect("scaffold executions");

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
        doc.put_object(&env, "prewarmed_packages", ObjType::List)
            .expect("scaffold env.prewarmed_packages");

        // trust/
        let trust = doc
            .put_object(&ROOT, "trust", ObjType::Map)
            .expect("scaffold trust");
        doc.put(&trust, "status", "no_dependencies")
            .expect("scaffold trust.status");
        doc.put(&trust, "needs_approval", false)
            .expect("scaffold trust.needs_approval");

        // comms/ — keyed by comm_id, tracks widget state
        doc.put_object(&ROOT, "comms", ObjType::Map)
            .expect("scaffold comms");

        // last_saved
        doc.put(&ROOT, "last_saved", ScalarValue::Null)
            .expect("scaffold last_saved");

        Self { doc }
    }

    /// Create a new `RuntimeStateDoc` with scaffolding and a custom actor.
    ///
    /// Used by the runtime agent to create its own doc with a unique actor
    /// that won't conflict with the coordinator's `"runtimed:state"` actor.
    /// The schema is identical — all scaffolding ops use the custom actor.
    #[allow(clippy::expect_used)]
    pub fn new_with_actor(actor_label: &str) -> Self {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(actor_label.as_bytes()));

        // Identical scaffolding as new(), but with a different actor
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
        doc.put(&kernel, "runtime_agent_id", "")
            .expect("scaffold kernel.runtime_agent_id");
        doc.put(&kernel, "starting_phase", "")
            .expect("scaffold kernel.starting_phase");

        let queue = doc
            .put_object(&ROOT, "queue", ObjType::Map)
            .expect("scaffold queue");
        doc.put(&queue, "executing", automerge::ScalarValue::Null)
            .expect("scaffold queue.executing");
        doc.put(
            &queue,
            "executing_execution_id",
            automerge::ScalarValue::Null,
        )
        .expect("scaffold queue.executing_execution_id");
        doc.put_object(&queue, "queued", ObjType::List)
            .expect("scaffold queue.queued");
        doc.put_object(&queue, "queued_execution_ids", ObjType::List)
            .expect("scaffold queue.queued_execution_ids");

        doc.put_object(&ROOT, "executions", ObjType::Map)
            .expect("scaffold executions");

        let env = doc
            .put_object(&ROOT, "env", ObjType::Map)
            .expect("scaffold env");
        doc.put(&env, "in_sync", true)
            .expect("scaffold env.in_sync");
        doc.put_object(&env, "added", ObjType::List)
            .expect("scaffold env.added");
        doc.put_object(&env, "removed", ObjType::List)
            .expect("scaffold env.removed");
        doc.put_object(&env, "prewarmed_packages", ObjType::List)
            .expect("scaffold env.prewarmed_packages");

        let trust = doc
            .put_object(&ROOT, "trust", ObjType::Map)
            .expect("scaffold trust");
        doc.put(&trust, "status", "no_dependencies")
            .expect("scaffold trust.status");
        doc.put(&trust, "needs_approval", false)
            .expect("scaffold trust.needs_approval");

        doc.put_object(&ROOT, "comms", ObjType::Map)
            .expect("scaffold comms");
        doc.put(&ROOT, "last_saved", automerge::ScalarValue::Null)
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

    /// Create a RuntimeStateDoc from a pre-existing Automerge document.
    ///
    /// Used by test fixtures and migration paths that have a saved state doc.
    pub fn from_doc(doc: AutoCommit) -> Self {
        Self { doc }
    }

    /// Access the underlying Automerge document (read-only).
    pub fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// Access the underlying Automerge document (mutable, for sync protocol).
    pub fn doc_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }

    // ── Fork + Merge ────────────────────────────────────────────────

    /// Fork the document at its current state.
    ///
    /// Returns a new `RuntimeStateDoc` whose underlying `AutoCommit` is an
    /// Automerge fork. Changes made on the fork are independent of the
    /// original — call [`merge`](Self::merge) to reconcile them.
    ///
    /// **Important:** Forks inherit the parent's actor ID. Call
    /// [`set_actor`](Self::set_actor) on the fork to assign a distinct
    /// identity (e.g., `"runtimed:state:cell-error"`) before making any
    /// mutations to avoid `DuplicateSeqNumber` errors on merge.
    pub fn fork(&mut self) -> Self {
        Self {
            doc: self.doc.fork(),
        }
    }

    /// Merge another `RuntimeStateDoc`'s changes into this one.
    ///
    /// Returns the change hashes that were applied. CRDT merge semantics
    /// apply — concurrent writes to different keys compose cleanly.
    pub fn merge(
        &mut self,
        other: &mut RuntimeStateDoc,
    ) -> Result<Vec<automerge::ChangeHash>, AutomergeError> {
        self.doc.merge(&mut other.doc)
    }

    /// Fork, apply mutations on the fork, and merge back.
    ///
    /// Convenience wrapper for synchronous fork+merge blocks. For async
    /// gaps (where an `.await` separates the read from the write), use
    /// [`fork`](Self::fork) and [`merge`](Self::merge) directly.
    pub fn fork_and_merge<F>(&mut self, f: F)
    where
        F: FnOnce(&mut RuntimeStateDoc),
    {
        let mut fork = self.fork();
        f(&mut fork);
        let _ = self.merge(&mut fork);
    }

    /// Round-trip save→load to rebuild internal automerge indices.
    ///
    /// Used after catching an automerge panic (upstream MissingOps bug in
    /// `collector.rs`). See `NotebookDoc::rebuild_from_save` for details.
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

    /// Set the actor identity for this document.
    ///
    /// Forks should call this with a distinct label so their changes are
    /// attributable and don't conflict with the parent's deterministic
    /// `"runtimed:state"` actor ID.
    pub fn set_actor(&mut self, label: &str) {
        self.doc.set_actor(ActorId::from(label.as_bytes()));
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

    /// Read an integer scalar from a map object, defaulting to 0.
    fn read_i64(&self, obj: &automerge::ObjId, key: &str) -> i64 {
        self.doc
            .get(obj, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Int(n) => Some(*n),
                    ScalarValue::Uint(n) => Some(*n as i64),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or(0)
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

    /// Read a List[Map] from a map object as `Vec<serde_json::Value>`.
    fn read_json_list(&self, obj: &automerge::ObjId, key: &str) -> Vec<serde_json::Value> {
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
            if let Some(val) = crate::read_json_value(&self.doc, &list_id, i) {
                out.push(val);
            }
        }
        out
    }

    // ── Granular setters (daemon calls these individually) ──────────

    /// Update trust state. Returns `true` if the doc was mutated.
    #[allow(clippy::expect_used)]
    pub fn set_trust(&mut self, status: &str, needs_approval: bool) -> bool {
        let trust = self.get_map("trust").expect("trust map must exist");
        let cur_status = self.read_str(&trust, "status");
        let cur_needs = self.read_bool(&trust, "needs_approval");

        if cur_status == status && cur_needs == needs_approval {
            return false;
        }

        self.doc
            .put(&trust, "status", status)
            .expect("put trust.status");
        self.doc
            .put(&trust, "needs_approval", needs_approval)
            .expect("put trust.needs_approval");
        true
    }

    /// Update kernel status. Returns `true` if the doc was mutated.
    ///
    /// Automatically clears `starting_phase` when transitioning away from `"starting"`.
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
        // Clear starting_phase when leaving "starting"
        if status != "starting" {
            let phase = self.read_str(&kernel, "starting_phase");
            if !phase.is_empty() {
                self.doc
                    .put(&kernel, "starting_phase", "")
                    .expect("clear kernel.starting_phase");
            }
        }
        true
    }

    /// Update the starting phase sub-status. Returns `true` if the doc was mutated.
    #[allow(clippy::expect_used)]
    pub fn set_starting_phase(&mut self, phase: &str) -> bool {
        let kernel = self.get_map("kernel").expect("kernel map must exist");
        let current = self.read_str(&kernel, "starting_phase");
        if current == phase {
            return false;
        }
        self.doc
            .put(&kernel, "starting_phase", phase)
            .expect("put kernel.starting_phase");
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

    /// Set the runtime agent ID that owns this kernel. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_runtime_agent_id(&mut self, runtime_agent_id: &str) -> bool {
        let kernel = self.get_map("kernel").expect("kernel map must exist");
        let current = self.read_str(&kernel, "runtime_agent_id");
        if current == runtime_agent_id {
            return false;
        }
        self.doc
            .put(&kernel, "runtime_agent_id", runtime_agent_id)
            .expect("put kernel.runtime_agent_id");
        true
    }

    /// Update queue state. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_queue(&mut self, executing: Option<&QueueEntry>, queued: &[QueueEntry]) -> bool {
        let queue = self.get_map("queue").expect("queue map must exist");
        let cur_exec_cid = self.read_opt_str(&queue, "executing");
        let cur_exec_eid = self.read_opt_str(&queue, "executing_execution_id");
        let cur_queued_cids = self.read_str_list(&queue, "queued");
        let cur_queued_eids = self.read_str_list(&queue, "queued_execution_ids");

        // Compare executing by both cell_id and execution_id
        let exec_match = match (&cur_exec_cid, executing) {
            (None, None) => true,
            (Some(cid), Some(entry)) => {
                cid == &entry.cell_id && cur_exec_eid.as_deref().unwrap_or("") == entry.execution_id
            }
            _ => false,
        };

        let queued_cids: Vec<&str> = queued.iter().map(|e| e.cell_id.as_str()).collect();
        let queued_eids: Vec<&str> = queued.iter().map(|e| e.execution_id.as_str()).collect();
        let cur_cid_refs: Vec<&str> = cur_queued_cids.iter().map(|s| s.as_str()).collect();
        let cur_eid_refs: Vec<&str> = cur_queued_eids.iter().map(|s| s.as_str()).collect();

        if exec_match && cur_cid_refs == queued_cids && cur_eid_refs == queued_eids {
            return false;
        }

        // Write executing cell_id and execution_id
        match executing {
            Some(entry) => {
                self.doc
                    .put(&queue, "executing", entry.cell_id.as_str())
                    .expect("put queue.executing");
                self.doc
                    .put(
                        &queue,
                        "executing_execution_id",
                        entry.execution_id.as_str(),
                    )
                    .expect("put queue.executing_execution_id");
            }
            None => {
                self.doc
                    .put(&queue, "executing", ScalarValue::Null)
                    .expect("put queue.executing null");
                self.doc
                    .put(&queue, "executing_execution_id", ScalarValue::Null)
                    .expect("put queue.executing_execution_id null");
            }
        }

        // Reuse existing list objects to avoid Automerge object churn.
        let cid_list = self
            .doc
            .get(&queue, "queued")
            .expect("get queue.queued")
            .map(|(_, id)| id)
            .expect("queued list must exist");
        let eid_list = self
            .doc
            .get(&queue, "queued_execution_ids")
            .expect("get queue.queued_execution_ids")
            .map(|(_, id)| id)
            .expect("queued_execution_ids list must exist");

        // Clear existing entries (reverse order for stable indices)
        let cid_len = self.doc.length(&cid_list);
        for i in (0..cid_len).rev() {
            self.doc
                .delete(&cid_list, i)
                .expect("delete queued cell_id");
        }
        let eid_len = self.doc.length(&eid_list);
        for i in (0..eid_len).rev() {
            self.doc
                .delete(&eid_list, i)
                .expect("delete queued execution_id");
        }

        // Insert new entries in both parallel lists
        for (i, entry) in queued.iter().enumerate() {
            self.doc
                .insert(&cid_list, i, entry.cell_id.as_str())
                .expect("insert queued cell_id");
            self.doc
                .insert(&eid_list, i, entry.execution_id.as_str())
                .expect("insert queued execution_id");
        }

        true
    }

    // ── Execution lifecycle ─────────────────────────────────────────

    /// Create a new execution entry with status "queued".
    ///
    /// Called by the daemon when `queue_cell()` generates an execution_id.
    #[allow(clippy::expect_used)]
    pub fn create_execution(&mut self, execution_id: &str, cell_id: &str) -> bool {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");

        // Don't overwrite if it already exists (idempotent queue)
        if self
            .doc
            .get(&executions, execution_id)
            .ok()
            .flatten()
            .is_some()
        {
            return false;
        }

        let entry = self
            .doc
            .put_object(&executions, execution_id, ObjType::Map)
            .expect("put execution entry");
        self.doc
            .put(&entry, "cell_id", cell_id)
            .expect("put execution.cell_id");
        self.doc
            .put(&entry, "status", "queued")
            .expect("put execution.status");
        self.doc
            .put(&entry, "execution_count", ScalarValue::Null)
            .expect("put execution.execution_count");
        self.doc
            .put(&entry, "success", ScalarValue::Null)
            .expect("put execution.success");
        self.doc
            .put_object(&entry, "outputs", ObjType::List)
            .expect("put execution.outputs");
        true
    }

    /// Create a new execution entry with source code and queue sequence number.
    ///
    /// Used by the coordinator to queue executions for the runtime agent. The
    /// source is stored as an audit log, and `seq` determines execution order.
    /// The runtime agent discovers new entries via CRDT sync and processes them
    /// in `seq` order.
    pub fn create_execution_with_source(
        &mut self,
        execution_id: &str,
        cell_id: &str,
        source: &str,
        seq: u64,
    ) -> bool {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");

        // Don't overwrite if it already exists (idempotent)
        if self
            .doc
            .get(&executions, execution_id)
            .ok()
            .flatten()
            .is_some()
        {
            return false;
        }

        let entry = self
            .doc
            .put_object(&executions, execution_id, ObjType::Map)
            .expect("put execution entry");
        self.doc
            .put(&entry, "cell_id", cell_id)
            .expect("put execution.cell_id");
        self.doc
            .put(&entry, "status", "queued")
            .expect("put execution.status");
        self.doc
            .put(&entry, "execution_count", ScalarValue::Null)
            .expect("put execution.execution_count");
        self.doc
            .put(&entry, "success", ScalarValue::Null)
            .expect("put execution.success");
        self.doc
            .put_object(&entry, "outputs", ObjType::List)
            .expect("put execution.outputs");
        self.doc
            .put(&entry, "source", source)
            .expect("put execution.source");
        self.doc
            .put(&entry, "seq", ScalarValue::Uint(seq))
            .expect("put execution.seq");
        true
    }

    /// Mark an execution as running.
    ///
    /// The execution_count is not known yet at this point — it arrives later
    /// from the kernel's `execute_input` message. Use [`set_execution_count`]
    /// to record it when it arrives.
    #[allow(clippy::expect_used)]
    pub fn set_execution_running(&mut self, execution_id: &str) -> bool {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");

        let Some((_, entry)) = self.doc.get(&executions, execution_id).ok().flatten() else {
            return false;
        };

        let cur_status = self.read_str(&entry, "status");
        if cur_status == "running" {
            return false;
        }

        self.doc
            .put(&entry, "status", "running")
            .expect("put execution.status");
        true
    }

    /// Set the execution_count for an execution (from kernel's execute_input).
    #[allow(clippy::expect_used)]
    pub fn set_execution_count(&mut self, execution_id: &str, execution_count: i64) -> bool {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");

        let Some((_, entry)) = self.doc.get(&executions, execution_id).ok().flatten() else {
            return false;
        };

        self.doc
            .put(&entry, "execution_count", execution_count)
            .expect("put execution.execution_count");
        true
    }

    /// Mark an execution as done or error.
    #[allow(clippy::expect_used)]
    pub fn set_execution_done(&mut self, execution_id: &str, success: bool) -> bool {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");

        let Some((_, entry)) = self.doc.get(&executions, execution_id).ok().flatten() else {
            return false;
        };

        let status = if success { "done" } else { "error" };
        self.doc
            .put(&entry, "status", status)
            .expect("put execution.status");
        self.doc
            .put(&entry, "success", success)
            .expect("put execution.success");
        true
    }

    /// Mark all in-flight executions (status "running" or "queued") as failed.
    /// Returns the number of executions marked. Used during kernel restart to
    /// catch any entries that the local KernelState doesn't know about (e.g.,
    /// entries created by CRDT sync that haven't been processed locally yet).
    #[allow(clippy::expect_used)]
    pub fn mark_inflight_executions_failed(&mut self) -> usize {
        let Some(executions) = self.get_map("executions") else {
            return 0;
        };

        let inflight: Vec<automerge::ObjId> =
            self.doc
                .map_range(&executions, ..)
                .filter_map(|item| {
                    if !matches!(item.value, automerge::ValueRef::Object(ObjType::Map)) {
                        return None;
                    }
                    let status = self.doc.get(item.id(), "status").ok().flatten().and_then(
                        |(v, _)| match v {
                            Value::Scalar(s) => s.to_str().map(|s| s.to_string()),
                            _ => None,
                        },
                    );
                    match status.as_deref() {
                        Some("running") | Some("queued") => Some(item.id()),
                        _ => None,
                    }
                })
                .collect();

        let count = inflight.len();
        for entry_id in inflight {
            self.doc
                .put(&entry_id, "status", "error")
                .expect("put execution.status");
            self.doc
                .put(&entry_id, "success", false)
                .expect("put execution.success");
        }
        count
    }

    /// Read a single execution's state.
    pub fn get_execution(&self, execution_id: &str) -> Option<ExecutionState> {
        let executions = self.get_map("executions")?;

        let (_, entry) = self.doc.get(&executions, execution_id).ok().flatten()?;

        let cell_id = self.read_str(&entry, "cell_id");
        let status = self.read_str(&entry, "status");

        let execution_count = self
            .doc
            .get(&entry, "execution_count")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Int(n) => Some(*n),
                    ScalarValue::Uint(n) => Some(*n as i64),
                    _ => None,
                },
                _ => None,
            });

        let success = self
            .doc
            .get(&entry, "success")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Boolean(b) => Some(*b),
                    _ => None,
                },
                _ => None,
            });

        let outputs = self.read_json_list(&entry, "outputs");

        let source = self.read_opt_str(&entry, "source");

        let seq = self
            .doc
            .get(&entry, "seq")
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                Value::Scalar(s) => match s.as_ref() {
                    ScalarValue::Uint(n) => Some(*n),
                    ScalarValue::Int(n) => Some(*n as u64),
                    _ => None,
                },
                _ => None,
            });

        Some(ExecutionState {
            cell_id,
            status,
            execution_count,
            success,
            outputs,
            source,
            seq,
        })
    }

    /// Get execution entries with `status == "queued"`, sorted by `seq`.
    ///
    /// Used by the runtime agent to discover new work via CRDT sync. Returns
    /// `(execution_id, ExecutionState)` pairs in execution order.
    pub fn get_queued_executions(&self) -> Vec<(String, ExecutionState)> {
        let state = self.read_state();
        let mut queued: Vec<(String, ExecutionState)> = state
            .executions
            .into_iter()
            .filter(|(_, exec)| exec.status == "queued")
            .collect();
        queued.sort_by_key(|(_, exec)| exec.seq.unwrap_or(u64::MAX));
        queued
    }

    // ── Output storage (keyed by execution_id) ──────────────────────

    /// Get the ObjId for the `executions/{execution_id}/outputs` list, if it exists.
    fn get_output_list(&self, execution_id: &str) -> Option<automerge::ObjId> {
        let executions = self.get_map("executions")?;
        let (_, entry) = self.doc.get(&executions, execution_id).ok().flatten()?;
        self.doc
            .get(&entry, "outputs")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                Value::Object(ObjType::List) => Some(id),
                _ => None,
            })
    }

    /// Ensure the `executions/{execution_id}/outputs` list exists, creating it if absent.
    /// Returns the ObjId of the list.
    #[allow(clippy::expect_used)]
    fn ensure_output_list(&mut self, execution_id: &str) -> automerge::ObjId {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");
        let (_, entry) = self
            .doc
            .get(&executions, execution_id)
            .ok()
            .flatten()
            .expect("execution entry must exist");
        match self.doc.get(&entry, "outputs").ok().flatten() {
            Some((Value::Object(ObjType::List), id)) => id,
            _ => self
                .doc
                .put_object(&entry, "outputs", ObjType::List)
                .expect("create outputs list on execution entry"),
        }
    }

    /// Append a single output manifest to the output list for an execution.
    ///
    /// The manifest is written as an Automerge Map at the list position.
    /// Creates the `outputs/{execution_id}` list if it doesn't exist.
    /// Returns the output index.
    #[allow(clippy::expect_used)]
    pub fn append_output(
        &mut self,
        execution_id: &str,
        manifest: &serde_json::Value,
    ) -> Result<usize, AutomergeError> {
        let list_id = self.ensure_output_list(execution_id);
        let len = self.doc.length(&list_id);
        crate::insert_json_at_index(&mut self.doc, &list_id, len, manifest)?;
        Ok(len)
    }

    /// Replace all outputs for an execution.
    ///
    /// Used during notebook load to populate outputs for synthetic execution_ids.
    #[allow(clippy::expect_used)]
    pub fn set_outputs(
        &mut self,
        execution_id: &str,
        manifests: &[serde_json::Value],
    ) -> Result<bool, AutomergeError> {
        let executions = self
            .get_map("executions")
            .expect("executions map must exist");
        let (_, entry) = self
            .doc
            .get(&executions, execution_id)
            .ok()
            .flatten()
            .expect("execution entry must exist");

        // Delete existing list and create fresh
        let _ = self.doc.delete(&entry, "outputs");
        let list_id = self.doc.put_object(&entry, "outputs", ObjType::List)?;
        for (i, manifest) in manifests.iter().enumerate() {
            crate::insert_json_at_index(&mut self.doc, &list_id, i, manifest)?;
        }
        Ok(true)
    }

    /// Reset an execution entry to a cleared state.
    ///
    /// Empties the outputs list and nulls `execution_count` and `success`, so
    /// the cell backed by this execution reads as "never run" from the
    /// frontend's point of view. Mirrors JupyterLab's `clearExecution()` which
    /// clears outputs *and* resets `executionCount` to `null`.
    ///
    /// Without nulling `execution_count`, the frontend's per-cell resolver
    /// (which walks all executions for a cell looking for a non-null count)
    /// would keep displaying the stale `[N]:` counter after a Clear Outputs.
    #[allow(clippy::expect_used)]
    pub fn clear_execution_outputs(&mut self, execution_id: &str) -> Result<bool, AutomergeError> {
        let Some(executions) = self.get_map("executions") else {
            return Ok(false);
        };
        let Some((_, entry)) = self.doc.get(&executions, execution_id).ok().flatten() else {
            return Ok(false);
        };
        // Replace with empty list
        let _ = self.doc.delete(&entry, "outputs");
        self.doc.put_object(&entry, "outputs", ObjType::List)?;
        // Null out execution_count and success so the cell reads as "never
        // run" — matches JupyterLab's clearExecution() semantics.
        self.doc.put(&entry, "execution_count", ScalarValue::Null)?;
        self.doc.put(&entry, "success", ScalarValue::Null)?;
        Ok(true)
    }

    /// Update or insert a stream output for an execution.
    ///
    /// If `known_state` is provided, validates that the output at the cached index
    /// still has the expected blob hash (via `text.blob`). If validation passes,
    /// replaces in place. If validation fails (hash mismatch, index out of bounds,
    /// or no state), appends a new output.
    ///
    /// Returns `(updated: bool, output_index: usize)` where `updated` is true if an
    /// existing output was replaced in place, false if a new output was appended.
    pub fn upsert_stream_output(
        &mut self,
        execution_id: &str,
        _stream_name: &str,
        manifest: &serde_json::Value,
        known_state: Option<&StreamOutputState>,
    ) -> Result<(bool, usize), AutomergeError> {
        let list_id = self.ensure_output_list(execution_id);
        let output_count = self.doc.length(&list_id);

        // Validate cached state if provided
        if let Some(state) = known_state {
            // Must be the last output — if something was appended after (e.g., stderr
            // between two stdout messages), we should append instead of updating
            if state.index + 1 == output_count {
                // Read the existing output and check text content ref against state.blob_hash.
                // ContentRef is either {"blob": "hash", "size": N} or {"inline": "text"}.
                // The cached blob_hash stores the blob hash or the inline content itself.
                if let Some(existing) = crate::read_json_value(&self.doc, &list_id, state.index) {
                    let current_id = existing.get("text").and_then(|t| {
                        t.get("blob")
                            .and_then(|b| b.as_str())
                            .or_else(|| t.get("inline").and_then(|i| i.as_str()))
                    });
                    if current_id == Some(&state.blob_hash) {
                        // Validated! Delete old and insert new at same index
                        self.doc.delete(&list_id, state.index)?;
                        crate::insert_json_at_index(
                            &mut self.doc,
                            &list_id,
                            state.index,
                            manifest,
                        )?;
                        return Ok((true, state.index));
                    }
                }
            }
            // Validation failed — fall through to append
        }

        // No valid state, append new output
        crate::insert_json_at_index(&mut self.doc, &list_id, output_count, manifest)?;
        Ok((false, output_count))
    }

    /// Replace an output at a specific index for an execution.
    ///
    /// Used by UpdateDisplayData handling for in-place manifest updates.
    /// Deletes the old entry and inserts the new manifest at the same position.
    pub fn replace_output(
        &mut self,
        execution_id: &str,
        output_idx: usize,
        manifest: &serde_json::Value,
    ) -> Result<bool, AutomergeError> {
        let Some(list_id) = self.get_output_list(execution_id) else {
            return Ok(false);
        };
        if output_idx >= self.doc.length(&list_id) {
            return Ok(false);
        }
        // Delete old entry and insert new manifest at same position
        self.doc.delete(&list_id, output_idx)?;
        crate::insert_json_at_index(&mut self.doc, &list_id, output_idx, manifest)?;
        Ok(true)
    }

    /// Read all outputs for an execution.
    pub fn get_outputs(&self, execution_id: &str) -> Vec<serde_json::Value> {
        let Some(list_id) = self.get_output_list(execution_id) else {
            return Vec::new();
        };
        let len = self.doc.length(&list_id);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let Some(val) = crate::read_json_value(&self.doc, &list_id, i) {
                out.push(val);
            }
        }
        out
    }

    /// Get all outputs across all executions.
    ///
    /// Returns `(execution_id, output_index, manifest)` triples.
    /// Used by UpdateDisplayData to find outputs with matching display_id.
    pub fn get_all_outputs(&self) -> Vec<(String, usize, serde_json::Value)> {
        let Some(executions) = self.get_map("executions") else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for exec_id in self.doc.keys(&executions) {
            if let Some((_, entry)) = self.doc.get(&executions, &exec_id).ok().flatten() {
                if let Some((Value::Object(ObjType::List), list_id)) =
                    self.doc.get(&entry, "outputs").ok().flatten()
                {
                    let len = self.doc.length(&list_id);
                    for i in 0..len {
                        if let Some(val) = crate::read_json_value(&self.doc, &list_id, i) {
                            results.push((exec_id.clone(), i, val));
                        }
                    }
                }
            }
        }
        results
    }

    // ── Execution lifecycle ────────────────────────────────────────

    /// Remove old executions, keeping the most recent `max` entries.
    ///
    /// Entries are removed in insertion order (oldest first). Always keeps
    /// the most recent execution for each cell_id regardless of `max`.
    #[allow(clippy::expect_used)]
    pub fn trim_executions(&mut self, max: usize) -> usize {
        let Some(executions) = self.get_map("executions") else {
            return 0;
        };

        // Collect all execution_ids and their cell_ids
        let keys: Vec<(String, String)> = self
            .doc
            .keys(&executions)
            .filter_map(|key| {
                let (_, entry) = self.doc.get(&executions, &key).ok().flatten()?;
                let cell_id = self.read_str(&entry, "cell_id");
                Some((key, cell_id))
            })
            .collect();

        let total = keys.len();
        if total <= max {
            return 0;
        }

        // Find the last occurrence index for each cell_id (those we must keep)
        let mut last_per_cell: HashMap<&str, usize> = HashMap::new();
        for (i, (_, cell_id)) in keys.iter().enumerate() {
            last_per_cell.insert(cell_id.as_str(), i);
        }

        // Remove oldest entries that aren't the last-per-cell, until we're at max
        let mut removed = 0;
        let to_remove = total - max;
        for (i, (exec_id, _)) in keys.iter().enumerate() {
            if removed >= to_remove {
                break;
            }
            // Skip if this is the most recent execution for its cell
            let cell_id = &keys[i].1;
            if last_per_cell.get(cell_id.as_str()) == Some(&i) {
                continue;
            }
            self.doc
                .delete(&executions, exec_id.as_str())
                .expect("delete execution entry");
            removed += 1;
        }
        removed
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

    /// Update prewarmed packages list. Returns `true` if mutated.
    #[allow(clippy::expect_used)]
    pub fn set_prewarmed_packages(&mut self, packages: &[String]) -> bool {
        let env = self.get_map("env").expect("env map must exist");
        let current = self.read_str_list(&env, "prewarmed_packages");
        if current == packages {
            return false;
        }

        let list = self
            .doc
            .get(&env, "prewarmed_packages")
            .expect("get env.prewarmed_packages")
            .map(|(_, id)| id)
            .expect("prewarmed_packages list must exist");
        let len = self.doc.length(&list);
        for i in (0..len).rev() {
            self.doc
                .delete(&list, i)
                .expect("delete prewarmed_packages entry");
        }
        for (i, pkg) in packages.iter().enumerate() {
            self.doc
                .insert(&list, i, pkg.as_str())
                .expect("insert prewarmed_packages pkg");
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

    // ── Comm lifecycle ────────────────────────────────────────────────

    /// Insert or replace a full comm entry (used on `comm_open`).
    #[allow(clippy::expect_used)]
    pub fn put_comm(
        &mut self,
        comm_id: &str,
        target_name: &str,
        model_module: &str,
        model_name: &str,
        state: &serde_json::Value,
        seq: u64,
    ) {
        let comms = self.get_map("comms").expect("comms map must exist");
        let entry = self
            .doc
            .put_object(&comms, comm_id, ObjType::Map)
            .expect("put comm entry");
        self.doc
            .put(&entry, "target_name", target_name)
            .expect("put comm.target_name");
        self.doc
            .put(&entry, "model_module", model_module)
            .expect("put comm.model_module");
        self.doc
            .put(&entry, "model_name", model_name)
            .expect("put comm.model_name");
        // Store state as a native Automerge map for per-property merge.
        // Safe: `entry` was just created by `put_object` above — no competing peers.
        #[allow(deprecated)]
        crate::put_json_at_key(&mut self.doc, &entry, "state", state).expect("put comm.state");
        self.doc
            .put(&entry, "seq", seq as i64)
            .expect("put comm.seq");
        self.doc
            .put_object(&entry, "outputs", ObjType::List)
            .expect("put comm.outputs");
        self.doc
            .put(&entry, "capture_msg_id", "")
            .expect("put comm.capture_msg_id");
    }

    /// Replace the full state for an existing comm.
    ///
    /// Set a single property in a comm's state map.
    ///
    /// Writes directly to `comms/{comm_id}/state/{key}` as a native
    /// Automerge value. This is the per-property write path used by
    /// the frontend for CRDT-based widget updates.
    #[allow(clippy::expect_used)]
    pub fn set_comm_state_property(
        &mut self,
        comm_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let Some((_, entry)) = self.doc.get(&comms, comm_id).ok().flatten() else {
            return false;
        };
        let Some((Value::Object(ObjType::Map), state_id)) =
            self.doc.get(&entry, "state").ok().flatten()
        else {
            return false;
        };
        crate::update_json_at_key(&mut self.doc, &state_id, key, value)
            .expect("update comm.state.key");
        true
    }

    /// Merge a state delta into a comm's state map, skipping no-op writes.
    ///
    /// For each key in `delta` (must be a JSON object), reads the current
    /// value from `comms/{comm_id}/state/{key}` and only writes if it
    /// differs. This suppresses echo-generated CRDT changes when the
    /// frontend writes a value → kernel echoes the same value back.
    ///
    /// Returns `true` if any property was actually changed.
    #[allow(clippy::expect_used)]
    pub fn merge_comm_state_delta(&mut self, comm_id: &str, delta: &serde_json::Value) -> bool {
        let Some(obj) = delta.as_object() else {
            return false;
        };
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let Some((_, entry)) = self.doc.get(&comms, comm_id).ok().flatten() else {
            return false;
        };
        let Some((Value::Object(ObjType::Map), state_id)) =
            self.doc.get(&entry, "state").ok().flatten()
        else {
            return false;
        };

        let mut any_changed = false;
        for (key, new_value) in obj {
            // For scalars, compare before writing to avoid no-op CRDT ops.
            // Objects/arrays are written unconditionally (rare in widget deltas).
            let should_write = match new_value {
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_) => {
                    let current = crate::read_json_value(&self.doc, &state_id, key.as_str());
                    current.as_ref() != Some(new_value)
                }
                _ => true,
            };
            if should_write {
                crate::update_json_at_key(&mut self.doc, &state_id, key, new_value)
                    .expect("update comm.state.key");
                any_changed = true;
            }
        }
        any_changed
    }

    /// Set or clear the capture_msg_id for an Output widget.
    ///
    /// When `msg_id` is non-empty, kernel outputs with matching
    /// `parent_header.msg_id` will be routed to this widget.
    /// Returns `false` if the comm doesn't exist.
    #[allow(clippy::expect_used)]
    pub fn set_comm_capture_msg_id(&mut self, comm_id: &str, msg_id: &str) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let Some((_, entry)) = self.doc.get(&comms, comm_id).ok().flatten() else {
            return false;
        };
        self.doc
            .put(&entry, "capture_msg_id", msg_id)
            .expect("put comm.capture_msg_id");
        true
    }

    /// Find the comm_id that is capturing outputs for a given msg_id.
    ///
    /// Scans all comms for a matching `capture_msg_id`. Single-depth only:
    /// returns the first match (most recently written wins via CRDT LWW).
    pub fn get_capture_widget(&self, msg_id: &str) -> Option<String> {
        if msg_id.is_empty() {
            return None;
        }
        let comms = self.get_map("comms")?;
        for comm_id in self.doc.keys(&comms) {
            if let Some((_, entry)) = self.doc.get(&comms, &comm_id).ok().flatten() {
                let capture = self.read_str(&entry, "capture_msg_id");
                if capture == msg_id {
                    return Some(comm_id);
                }
            }
        }
        None
    }

    /// Remove a comm entry (used on `comm_close`).
    ///
    /// Returns `false` if the comm doesn't exist.
    #[allow(clippy::expect_used)]
    pub fn remove_comm(&mut self, comm_id: &str) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        if self.doc.get(&comms, comm_id).ok().flatten().is_none() {
            return false;
        }
        self.doc.delete(&comms, comm_id).expect("delete comm");
        true
    }

    /// Remove all comm entries (used on kernel shutdown/restart).
    ///
    /// Returns `false` if there were no comms to remove.
    #[allow(clippy::expect_used)]
    pub fn clear_comms(&mut self) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let keys: Vec<String> = self.doc.keys(&comms).collect();
        if keys.is_empty() {
            return false;
        }
        for key in keys {
            self.doc.delete(&comms, &key).expect("delete comm entry");
        }
        true
    }

    /// Read a single comm entry.
    pub fn get_comm(&self, comm_id: &str) -> Option<CommDocEntry> {
        let comms = self.get_map("comms")?;
        let (_, entry) = self.doc.get(&comms, comm_id).ok().flatten()?;
        // Read state as native Automerge map → serde_json::Value
        let state = crate::read_json_value(&self.doc, &entry, "state")
            .unwrap_or_else(|| serde_json::json!({}));
        Some(CommDocEntry {
            target_name: self.read_str(&entry, "target_name"),
            model_module: self.read_str(&entry, "model_module"),
            model_name: self.read_str(&entry, "model_name"),
            state,
            outputs: self.read_json_list(&entry, "outputs"),
            seq: self.read_i64(&entry, "seq") as u64,
            capture_msg_id: self.read_str(&entry, "capture_msg_id"),
        })
    }

    /// Append an output manifest to a comm's outputs list (OutputModel widgets).
    ///
    /// Returns `false` if the comm doesn't exist.
    #[allow(clippy::expect_used)]
    pub fn append_comm_output(&mut self, comm_id: &str, manifest: &serde_json::Value) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let Some((_, entry)) = self.doc.get(&comms, comm_id).ok().flatten() else {
            return false;
        };
        let Some((Value::Object(ObjType::List), list_id)) =
            self.doc.get(&entry, "outputs").ok().flatten()
        else {
            return false;
        };
        let len = self.doc.length(&list_id);
        crate::insert_json_at_index(&mut self.doc, &list_id, len, manifest)
            .expect("append comm output");
        true
    }

    /// Clear a comm's outputs list (OutputModel widgets).
    ///
    /// Returns `false` if the comm doesn't exist or outputs is already empty.
    #[allow(clippy::expect_used)]
    pub fn clear_comm_outputs(&mut self, comm_id: &str) -> bool {
        let Some(comms) = self.get_map("comms") else {
            return false;
        };
        let Some((_, entry)) = self.doc.get(&comms, comm_id).ok().flatten() else {
            return false;
        };
        let Some((Value::Object(ObjType::List), list_id)) =
            self.doc.get(&entry, "outputs").ok().flatten()
        else {
            return false;
        };
        let len = self.doc.length(&list_id);
        if len == 0 {
            return false;
        }
        for i in (0..len).rev() {
            self.doc.delete(&list_id, i).expect("clear comm output");
        }
        true
    }

    // ── Full state read ─────────────────────────────────────────────

    /// Read the full runtime state snapshot.
    pub fn read_state(&self) -> RuntimeState {
        let kernel = self.get_map("kernel");
        let queue = self.get_map("queue");
        let env = self.get_map("env");
        let trust = self.get_map("trust");

        let kernel_state = kernel
            .as_ref()
            .map(|k| KernelState {
                status: self.read_str(k, "status"),
                starting_phase: self.read_str(k, "starting_phase"),
                name: self.read_str(k, "name"),
                language: self.read_str(k, "language"),
                env_source: self.read_str(k, "env_source"),
                runtime_agent_id: self.read_str(k, "runtime_agent_id"),
            })
            .unwrap_or_default();

        let queue_state = queue
            .as_ref()
            .map(|q| {
                let executing_cid = self.read_opt_str(q, "executing");
                let executing_eid = self.read_opt_str(q, "executing_execution_id");
                let queued_cids = self.read_str_list(q, "queued");
                let queued_eids = self.read_str_list(q, "queued_execution_ids");

                QueueState {
                    executing: executing_cid.map(|cid| QueueEntry {
                        cell_id: cid,
                        execution_id: executing_eid.unwrap_or_default(),
                    }),
                    queued: queued_cids
                        .into_iter()
                        .zip(
                            queued_eids
                                .into_iter()
                                .chain(std::iter::repeat(String::new())),
                        )
                        .map(|(cid, eid)| QueueEntry {
                            cell_id: cid,
                            execution_id: eid,
                        })
                        .collect(),
                }
            })
            .unwrap_or_default();

        // Read executions map
        let executions = self
            .get_map("executions")
            .map(|exec_obj| {
                let mut map = HashMap::new();
                for key in self.doc.keys(&exec_obj) {
                    if let Some(es) = self.get_execution(&key) {
                        map.insert(key, es);
                    }
                }
                map
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
                prewarmed_packages: self.read_str_list(e, "prewarmed_packages"),
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

        let trust_state = trust
            .as_ref()
            .map(|t| TrustRuntimeState {
                status: self.read_str(t, "status"),
                needs_approval: self.read_bool(t, "needs_approval"),
            })
            .unwrap_or_default();

        // Read comms map
        let comms = self
            .get_map("comms")
            .map(|comms_obj| {
                let mut map = HashMap::new();
                for key in self.doc.keys(&comms_obj) {
                    if let Some(entry) = self.get_comm(&key) {
                        map.insert(key, entry);
                    }
                }
                map
            })
            .unwrap_or_default();

        RuntimeState {
            kernel: kernel_state,
            queue: queue_state,
            env: env_state,
            trust: trust_state,
            last_saved,
            executions,
            comms,
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

    /// Receive a sync message accepting client writes.
    ///
    /// Unlike `receive_sync_message()` which strips client changes, this
    /// accepts the full message including any mutations the client made
    /// (e.g., widget state updates written to `comms/*/state/*`).
    ///
    /// Returns `true` if the document heads changed (i.e., client sent
    /// new changes, not just a handshake).
    pub fn receive_sync_message_with_changes(
        &mut self,
        peer_state: &mut sync::State,
        message: sync::Message,
    ) -> Result<bool, AutomergeError> {
        let heads_before = self.doc.get_heads();
        self.doc.sync().receive_sync_message(peer_state, message)?;
        let heads_after = self.doc.get_heads();
        Ok(heads_before != heads_after)
    }
}

// ── Output diff utility ─────────────────────────────────────────────

/// Diff execution outputs between a previous snapshot and the current state.
///
/// Returns `(changed_cell_ids, new_snapshot)` where:
/// - `changed_cell_ids` lists cells whose outputs changed
/// - `new_snapshot` is the updated prev_execution_outputs for the next diff
///
/// Used by the WASM handle to detect mid-execution output changes
/// (stream append, display update, error) without re-materializing
/// all cells.
pub fn diff_execution_outputs(
    prev: &HashMap<String, Vec<serde_json::Value>>,
    current_executions: &HashMap<String, ExecutionState>,
) -> (Vec<String>, HashMap<String, Vec<serde_json::Value>>) {
    let mut changed_cells = Vec::new();

    for (eid, exec) in current_executions {
        let outputs_changed = match prev.get(eid) {
            None => !exec.outputs.is_empty(),
            Some(prev_outputs) => prev_outputs != &exec.outputs,
        };
        if outputs_changed {
            changed_cells.push(exec.cell_id.clone());
        }
    }

    // Keep ALL executions (even with empty outputs) so the next diff
    // correctly detects transitions from [] → [hash].
    let new_snapshot: HashMap<String, Vec<serde_json::Value>> = current_executions
        .iter()
        .map(|(eid, e)| (eid.clone(), e.outputs.clone()))
        .collect();

    (changed_cells, new_snapshot)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a stream output manifest with a blob ContentRef for tests.
    fn test_stream(blob: &str) -> serde_json::Value {
        serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": {"blob": blob, "size": blob.len()}
        })
    }

    /// Create a stream output manifest with an inline ContentRef for tests.
    fn test_stream_inline(text: &str) -> serde_json::Value {
        serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": {"inline": text}
        })
    }

    /// Create a display_data output manifest for tests.
    fn test_display(blob: &str) -> serde_json::Value {
        serde_json::json!({
            "output_type": "display_data",
            "data": {"text/plain": {"blob": blob, "size": blob.len()}}
        })
    }

    #[test]
    fn test_new_doc_has_default_state() {
        let doc = RuntimeStateDoc::new();
        let state = doc.read_state();
        // Note: new() scaffolds trust.status as "no_dependencies" which matches
        // TrustRuntimeState::default() (empty string), so we compare fields individually.
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
        assert_eq!(state.trust.status, "no_dependencies");
        assert!(!state.trust.needs_approval);
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
        let exec = QueueEntry {
            cell_id: "cell-1".to_string(),
            execution_id: "exec-1".to_string(),
        };
        let queued = vec![
            QueueEntry {
                cell_id: "cell-2".to_string(),
                execution_id: "exec-2".to_string(),
            },
            QueueEntry {
                cell_id: "cell-3".to_string(),
                execution_id: "exec-3".to_string(),
            },
        ];
        assert!(doc.set_queue(Some(&exec), &queued));

        let state = doc.read_state();
        assert_eq!(state.queue.executing.as_ref().unwrap().cell_id, "cell-1");
        assert_eq!(
            state.queue.executing.as_ref().unwrap().execution_id,
            "exec-1"
        );
        assert_eq!(state.queue.queued.len(), 2);
        assert_eq!(state.queue.queued[0].cell_id, "cell-2");
        assert_eq!(state.queue.queued[0].execution_id, "exec-2");
        assert_eq!(state.queue.queued[1].cell_id, "cell-3");
        assert_eq!(state.queue.queued[1].execution_id, "exec-3");
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
    fn test_set_trust() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.set_trust("untrusted", true));
        let state = doc.read_state();
        assert_eq!(state.trust.status, "untrusted");
        assert!(state.trust.needs_approval);

        assert!(doc.set_trust("trusted", false));
        let state = doc.read_state();
        assert_eq!(state.trust.status, "trusted");
        assert!(!state.trust.needs_approval);
    }

    #[test]
    fn test_set_starting_phase() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.set_kernel_status("starting"));
        assert!(doc.set_starting_phase("resolving"));
        assert_eq!(doc.read_state().kernel.starting_phase, "resolving");

        assert!(doc.set_starting_phase("launching"));
        assert_eq!(doc.read_state().kernel.starting_phase, "launching");

        // Dedup: same phase is no-op
        assert!(!doc.set_starting_phase("launching"));
    }

    #[test]
    fn test_kernel_status_clears_starting_phase() {
        let mut doc = RuntimeStateDoc::new();
        doc.set_kernel_status("starting");
        doc.set_starting_phase("connecting");
        assert_eq!(doc.read_state().kernel.starting_phase, "connecting");

        // Transitioning to "idle" should clear starting_phase
        doc.set_kernel_status("idle");
        assert_eq!(doc.read_state().kernel.starting_phase, "");

        // Same for error
        doc.set_kernel_status("starting");
        doc.set_starting_phase("launching");
        doc.set_kernel_status("error");
        assert_eq!(doc.read_state().kernel.starting_phase, "");
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
        let exec = QueueEntry {
            cell_id: "x".to_string(),
            execution_id: "e1".to_string(),
        };
        let q = vec![QueueEntry {
            cell_id: "a".to_string(),
            execution_id: "e2".to_string(),
        }];
        assert!(doc.set_queue(Some(&exec), &q));
        assert!(!doc.set_queue(Some(&exec), &q));

        // Same for env — defaults are (true, [], [], false, false),
        // so use non-default values to get an initial mutation.
        assert!(doc.set_env_sync(false, &[], &[], false, false));
        assert!(!doc.set_env_sync(false, &[], &[], false, false));

        // Same for trust — default is ("no_dependencies", false)
        assert!(doc.set_trust("trusted", false));
        assert!(!doc.set_trust("trusted", false));

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
            Some(&QueueEntry {
                cell_id: "cell-1".to_string(),
                execution_id: "exec-1".to_string(),
            }),
            &[
                QueueEntry {
                    cell_id: "cell-2".to_string(),
                    execution_id: "exec-2".to_string(),
                },
                QueueEntry {
                    cell_id: "cell-3".to_string(),
                    execution_id: "exec-3".to_string(),
                },
            ],
        );
        daemon_doc.set_env_sync(
            false,
            &["numpy".to_string()],
            &["scipy".to_string()],
            true,
            false,
        );
        daemon_doc.set_trust("untrusted", true);
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
        assert_eq!(
            client_state.queue.executing.as_ref().unwrap().cell_id,
            "cell-1"
        );
        assert_eq!(
            client_state.queue.executing.as_ref().unwrap().execution_id,
            "exec-1"
        );
        assert_eq!(client_state.queue.queued.len(), 2);
        assert!(!client_state.env.in_sync);
        assert_eq!(client_state.trust.status, "untrusted");
        assert!(client_state.trust.needs_approval);
        assert_eq!(
            client_state.last_saved,
            Some("2025-01-15T12:00:00Z".to_string()),
        );
    }

    // ── Execution lifecycle tests ───────────────────────────────────

    #[test]
    fn test_create_execution() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.create_execution("exec-1", "cell-1"));

        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.cell_id, "cell-1");
        assert_eq!(es.status, "queued");
        assert_eq!(es.execution_count, None);
        assert_eq!(es.success, None);
    }

    #[test]
    fn test_create_execution_with_source() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.create_execution_with_source("exec-1", "cell-1", "x = 42", 0));

        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.cell_id, "cell-1");
        assert_eq!(es.status, "queued");
        assert_eq!(es.source, Some("x = 42".to_string()));
        assert_eq!(es.seq, Some(0));
    }

    #[test]
    fn test_get_queued_executions_sorted_by_seq() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution_with_source("exec-3", "cell-3", "z = 3", 2);
        doc.create_execution_with_source("exec-1", "cell-1", "x = 1", 0);
        doc.create_execution_with_source("exec-2", "cell-2", "y = 2", 1);

        let queued = doc.get_queued_executions();
        assert_eq!(queued.len(), 3);
        assert_eq!(queued[0].0, "exec-1");
        assert_eq!(queued[1].0, "exec-2");
        assert_eq!(queued[2].0, "exec-3");

        // Transition one to running — should no longer appear
        doc.set_execution_running("exec-1");
        let queued = doc.get_queued_executions();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].0, "exec-2");
    }

    #[test]
    fn test_create_execution_idempotent() {
        let mut doc = RuntimeStateDoc::new();
        assert!(doc.create_execution("exec-1", "cell-1"));
        // Second create for same execution_id is a no-op
        assert!(!doc.create_execution("exec-1", "cell-1"));
    }

    #[test]
    fn test_execution_lifecycle_success() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");

        // queued → running
        assert!(doc.set_execution_running("exec-1"));
        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.status, "running");
        assert_eq!(es.execution_count, None);

        // Set execution_count separately (from kernel execute_input)
        assert!(doc.set_execution_count("exec-1", 5));
        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.execution_count, Some(5));

        // running → done
        assert!(doc.set_execution_done("exec-1", true));
        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.status, "done");
        assert_eq!(es.success, Some(true));
    }

    #[test]
    fn test_execution_lifecycle_error() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.set_execution_running("exec-1");
        doc.set_execution_count("exec-1", 3);

        // running → error
        assert!(doc.set_execution_done("exec-1", false));
        let es = doc.get_execution("exec-1").unwrap();
        assert_eq!(es.status, "error");
        assert_eq!(es.success, Some(false));
    }

    #[test]
    fn test_get_execution_nonexistent() {
        let doc = RuntimeStateDoc::new();
        assert!(doc.get_execution("nope").is_none());
    }

    #[test]
    fn test_mark_inflight_executions_failed() {
        let mut doc = RuntimeStateDoc::new();
        // One running, one queued, one already done
        doc.create_execution("exec-running", "cell-1");
        doc.set_execution_running("exec-running");

        doc.create_execution("exec-queued", "cell-2");

        doc.create_execution("exec-done", "cell-3");
        doc.set_execution_running("exec-done");
        doc.set_execution_done("exec-done", true);

        assert_eq!(doc.get_execution("exec-running").unwrap().status, "running");
        assert_eq!(doc.get_execution("exec-queued").unwrap().status, "queued");
        assert_eq!(doc.get_execution("exec-done").unwrap().status, "done");

        let marked = doc.mark_inflight_executions_failed();
        assert_eq!(marked, 2);

        assert_eq!(doc.get_execution("exec-running").unwrap().status, "error");
        assert_eq!(
            doc.get_execution("exec-running").unwrap().success,
            Some(false)
        );
        assert_eq!(doc.get_execution("exec-queued").unwrap().status, "error");
        assert_eq!(
            doc.get_execution("exec-queued").unwrap().success,
            Some(false)
        );
        // Done execution should be untouched
        assert_eq!(doc.get_execution("exec-done").unwrap().status, "done");
        assert_eq!(doc.get_execution("exec-done").unwrap().success, Some(true));
    }

    #[test]
    fn test_mark_inflight_noop_when_all_done() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.set_execution_running("exec-1");
        doc.set_execution_done("exec-1", true);

        assert_eq!(doc.mark_inflight_executions_failed(), 0);
    }

    #[test]
    fn test_set_execution_running_idempotent() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        assert!(doc.set_execution_running("exec-1"));
        // Already running — no-op
        assert!(!doc.set_execution_running("exec-1"));
    }

    #[test]
    fn test_executions_in_read_state() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.set_execution_running("exec-1");
        doc.set_execution_count("exec-1", 7);
        doc.create_execution("exec-2", "cell-2");

        let state = doc.read_state();
        assert_eq!(state.executions.len(), 2);

        let e1 = &state.executions["exec-1"];
        assert_eq!(e1.cell_id, "cell-1");
        assert_eq!(e1.status, "running");
        assert_eq!(e1.execution_count, Some(7));

        let e2 = &state.executions["exec-2"];
        assert_eq!(e2.cell_id, "cell-2");
        assert_eq!(e2.status, "queued");
    }

    #[test]
    fn test_trim_executions() {
        let mut doc = RuntimeStateDoc::new();
        // Create 5 executions for 2 cells
        doc.create_execution("e1", "cell-a");
        doc.create_execution("e2", "cell-a");
        doc.create_execution("e3", "cell-b");
        doc.create_execution("e4", "cell-a");
        doc.create_execution("e5", "cell-b");

        // Trim to 3 — should keep e4 (latest cell-a), e5 (latest cell-b),
        // and one more. Oldest non-latest-per-cell are removed first.
        let removed = doc.trim_executions(3);
        assert!(removed > 0);

        let state = doc.read_state();
        // Must keep latest per cell: e4 (cell-a) and e5 (cell-b)
        assert!(state.executions.contains_key("e4"));
        assert!(state.executions.contains_key("e5"));
        assert!(state.executions.len() <= 3);
    }

    #[test]
    fn test_trim_executions_noop_when_under_max() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("e1", "cell-1");
        doc.create_execution("e2", "cell-2");
        assert_eq!(doc.trim_executions(10), 0);
        assert_eq!(doc.read_state().executions.len(), 2);
    }

    #[test]
    fn test_execution_lifecycle_syncs_between_docs() {
        let mut daemon_doc = RuntimeStateDoc::new();
        daemon_doc.create_execution("exec-1", "cell-1");
        daemon_doc.set_execution_running("exec-1");
        daemon_doc.set_execution_count("exec-1", 3);
        daemon_doc.set_execution_done("exec-1", true);

        // Sync to client (use raw automerge sync for client receive,
        // change-stripping receive for daemon — matches real topology).
        let mut client_doc = RuntimeStateDoc::new_empty();
        let mut daemon_sync = sync::State::new();
        let mut client_sync = sync::State::new();

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

        let client_state = client_doc.read_state();
        assert_eq!(client_state.executions.len(), 1);
        let es = &client_state.executions["exec-1"];
        assert_eq!(es.cell_id, "cell-1");
        assert_eq!(es.status, "done");
        assert_eq!(es.success, Some(true));
        assert_eq!(es.execution_count, Some(3));
    }

    // ── Fork+Merge Tests ────────────────────────────────────────────

    #[test]
    fn test_fork_and_merge_basic() {
        let mut doc = RuntimeStateDoc::new();

        let mut fork = doc.fork();
        fork.set_actor("runtimed:state:test");

        let entry = QueueEntry {
            cell_id: "cell-1".to_string(),
            execution_id: "exec-1".to_string(),
        };
        fork.set_queue(Some(&entry), &[]);

        doc.merge(&mut fork).unwrap();

        let state = doc.read_state();
        assert_eq!(
            state.queue.executing.as_ref().map(|e| e.cell_id.as_str()),
            Some("cell-1")
        );
    }

    #[test]
    fn test_fork_and_merge_concurrent_writes() {
        // Fork, write queue on fork AND kernel status on original,
        // merge — both changes should be present.
        let mut doc = RuntimeStateDoc::new();

        let mut fork = doc.fork();
        fork.set_actor("runtimed:state:test");

        // Write queue on fork
        let entry = QueueEntry {
            cell_id: "cell-1".to_string(),
            execution_id: "exec-1".to_string(),
        };
        fork.set_queue(Some(&entry), &[]);

        // Write kernel status on original (concurrent)
        doc.set_kernel_status("busy");

        // Merge — both changes should compose
        doc.merge(&mut fork).unwrap();

        let state = doc.read_state();
        assert_eq!(state.kernel.status, "busy");
        assert_eq!(
            state.queue.executing.as_ref().map(|e| e.cell_id.as_str()),
            Some("cell-1")
        );
    }

    #[test]
    fn test_fork_actor_distinct() {
        let mut doc = RuntimeStateDoc::new();
        let mut fork = doc.fork();

        // Fork inherits parent actor — must set a distinct one
        fork.set_actor("runtimed:state:cell-error");

        // Verify the fork's actor is different from the parent
        let parent_actor = format!("{}", doc.doc().get_actor());
        let fork_actor = format!("{}", fork.doc().get_actor());
        assert_ne!(parent_actor, fork_actor);
    }

    #[test]
    fn test_fork_and_merge_closure() {
        let mut doc = RuntimeStateDoc::new();

        doc.fork_and_merge(|fork| {
            fork.set_actor("runtimed:state:test");
            fork.set_kernel_status("error");
            fork.set_queue(None, &[]);
            fork.set_execution_done("exec-1", false);
        });

        let state = doc.read_state();
        assert_eq!(state.kernel.status, "error");
        assert!(state.queue.executing.is_none());
    }

    // ── Output storage tests ───────────────────────────────────────

    #[test]
    fn test_append_output() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m_a = test_stream("hash-a");
        let m_b = test_stream("hash-b");
        let idx0 = doc.append_output("exec-1", &m_a).unwrap();
        assert_eq!(idx0, 0);
        let idx1 = doc.append_output("exec-1", &m_b).unwrap();
        assert_eq!(idx1, 1);

        assert_eq!(doc.get_outputs("exec-1"), vec![m_a, m_b]);
    }

    #[test]
    fn test_set_outputs() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.append_output("exec-1", &test_stream("old-hash"))
            .unwrap();

        let manifests = vec![test_display("h1"), test_display("h2"), test_display("h3")];
        doc.set_outputs("exec-1", &manifests).unwrap();

        assert_eq!(doc.get_outputs("exec-1"), manifests);
    }

    #[test]
    fn test_clear_execution_outputs() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.append_output("exec-1", &test_stream("hash-a")).unwrap();
        doc.append_output("exec-1", &test_stream("hash-b")).unwrap();

        assert!(doc.clear_execution_outputs("exec-1").unwrap());
        assert!(doc.get_outputs("exec-1").is_empty());

        // Clearing nonexistent is a no-op
        assert!(!doc.clear_execution_outputs("nope").unwrap());
    }

    #[test]
    fn test_clear_execution_outputs_resets_count_and_success() {
        // Regression: Clear Outputs must also null execution_count and
        // success, or the cell keeps showing its stale [N]: counter
        // (matches JupyterLab's clearExecution()).
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.set_execution_count("exec-1", 7);
        doc.set_execution_done("exec-1", true);
        doc.append_output("exec-1", &test_stream("hash-a")).unwrap();

        let before = doc.get_execution("exec-1").unwrap();
        assert_eq!(before.execution_count, Some(7));
        assert_eq!(before.success, Some(true));

        assert!(doc.clear_execution_outputs("exec-1").unwrap());

        let after = doc.get_execution("exec-1").unwrap();
        assert!(after.outputs.is_empty());
        assert_eq!(after.execution_count, None);
        assert_eq!(after.success, None);
        // Status is intentionally left as-is; "cleared" is a transient UX
        // state, not a new lifecycle phase — the next run overwrites it.
    }

    #[test]
    fn test_replace_output() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m_a = test_display("hash-a");
        let m_b = test_display("hash-b");
        let m_c = test_display("hash-c");
        let m_d = test_display("hash-d");
        doc.append_output("exec-1", &m_a).unwrap();
        doc.append_output("exec-1", &m_b).unwrap();

        assert!(doc.replace_output("exec-1", 1, &m_c).unwrap());
        assert_eq!(doc.get_outputs("exec-1"), vec![m_a, m_c]);

        // Out of bounds
        assert!(!doc.replace_output("exec-1", 5, &m_d).unwrap());
        // Nonexistent execution
        assert!(!doc.replace_output("nope", 0, &m_d).unwrap());
    }

    #[test]
    fn test_upsert_stream_output_append() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");

        let manifest = test_stream("hash-a");
        // No known state → append
        let (updated, idx) = doc
            .upsert_stream_output("exec-1", "stdout", &manifest, None)
            .unwrap();
        assert!(!updated);
        assert_eq!(idx, 0);
        assert_eq!(doc.get_outputs("exec-1"), vec![manifest]);
    }

    #[test]
    fn test_upsert_stream_output_update_in_place() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m_a = test_stream("hash-a");
        doc.append_output("exec-1", &m_a).unwrap();

        let state = StreamOutputState {
            index: 0,
            blob_hash: "hash-a".to_string(),
        };
        let m_b = test_stream("hash-b");
        let (updated, idx) = doc
            .upsert_stream_output("exec-1", "stdout", &m_b, Some(&state))
            .unwrap();
        assert!(updated);
        assert_eq!(idx, 0);
        assert_eq!(doc.get_outputs("exec-1"), vec![m_b]);
    }

    #[test]
    fn test_upsert_stream_output_inline_update_in_place() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m_a = test_stream_inline("***");
        doc.append_output("exec-1", &m_a).unwrap();

        // blob_hash stores the inline content itself for inline ContentRefs
        let state = StreamOutputState {
            index: 0,
            blob_hash: "***".to_string(),
        };
        let m_b = test_stream_inline("******");
        let (updated, idx) = doc
            .upsert_stream_output("exec-1", "stdout", &m_b, Some(&state))
            .unwrap();
        assert!(updated);
        assert_eq!(idx, 0);
        assert_eq!(doc.get_outputs("exec-1"), vec![m_b]);
    }

    #[test]
    fn test_upsert_stream_output_inline_to_blob_transition() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        // Start with inline content
        let m_a = test_stream_inline("small");
        doc.append_output("exec-1", &m_a).unwrap();

        let state = StreamOutputState {
            index: 0,
            blob_hash: "small".to_string(),
        };
        // Transition to blob when content grows past threshold
        let m_b = test_stream("blob-hash-after-growth");
        let (updated, idx) = doc
            .upsert_stream_output("exec-1", "stdout", &m_b, Some(&state))
            .unwrap();
        assert!(updated);
        assert_eq!(idx, 0);
        assert_eq!(doc.get_outputs("exec-1"), vec![m_b]);
    }

    #[test]
    fn test_upsert_stream_output_hash_mismatch_appends() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m_a = test_stream("hash-a");
        doc.append_output("exec-1", &m_a).unwrap();

        let state = StreamOutputState {
            index: 0,
            blob_hash: "wrong-hash".to_string(),
        };
        let m_b = test_stream("hash-b");
        let (updated, idx) = doc
            .upsert_stream_output("exec-1", "stdout", &m_b, Some(&state))
            .unwrap();
        assert!(!updated);
        assert_eq!(idx, 1);
        assert_eq!(doc.get_outputs("exec-1"), vec![m_a, m_b]);
    }

    #[test]
    fn test_get_all_outputs() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.create_execution("exec-2", "cell-2");
        let m1 = test_stream("h1");
        let m2 = test_stream("h2");
        let m3 = test_stream("h3");
        doc.append_output("exec-1", &m1).unwrap();
        doc.append_output("exec-1", &m2).unwrap();
        doc.append_output("exec-2", &m3).unwrap();

        let all = doc.get_all_outputs();
        assert_eq!(all.len(), 3);

        // Check that all expected entries are present (order across execution_ids
        // depends on Automerge key iteration order, so use contains)
        assert!(all.contains(&("exec-1".to_string(), 0, m1)));
        assert!(all.contains(&("exec-1".to_string(), 1, m2)));
        assert!(all.contains(&("exec-2".to_string(), 0, m3)));
    }

    #[test]
    fn test_inline_outputs_in_execution_state() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        doc.create_execution("exec-2", "cell-2");
        let m1 = test_stream("h1");
        let m2 = test_stream("h2");
        let m3 = test_stream("h3");
        doc.append_output("exec-1", &m1).unwrap();
        doc.append_output("exec-1", &m2).unwrap();
        doc.append_output("exec-2", &m3).unwrap();

        let state = doc.read_state();
        assert_eq!(state.executions["exec-1"].outputs, vec![m1, m2]);
        assert_eq!(state.executions["exec-2"].outputs, vec![m3]);
    }

    #[test]
    fn test_trim_executions_also_trims_outputs() {
        let mut doc = RuntimeStateDoc::new();
        // Create 5 executions with outputs
        for i in 1..=5 {
            let eid = format!("e{i}");
            let cid = if i % 2 == 0 { "cell-a" } else { "cell-b" };
            doc.create_execution(&eid, cid);
            doc.append_output(&eid, &test_stream(&format!("hash-{i}")))
                .unwrap();
        }

        // Trim to 3
        let removed = doc.trim_executions(3);
        assert!(removed > 0);

        let state = doc.read_state();
        // Outputs for trimmed executions should be gone
        for eid in state.executions.keys() {
            assert!(
                !doc.get_outputs(eid).is_empty(),
                "surviving execution {eid} should still have outputs"
            );
        }
        // Trimmed executions should not have output entries
        for eid in ["e1", "e2", "e3", "e4", "e5"] {
            if !state.executions.contains_key(eid) {
                assert!(
                    doc.get_outputs(eid).is_empty(),
                    "trimmed execution {eid} should have no outputs"
                );
            }
        }
    }

    #[test]
    fn test_outputs_sync_between_docs() {
        let mut daemon_doc = RuntimeStateDoc::new();
        daemon_doc.create_execution("exec-1", "cell-1");
        daemon_doc.create_execution("exec-2", "cell-2");
        let m1 = test_stream("h1");
        let m2 = test_stream("h2");
        let m3 = test_stream("h3");
        daemon_doc.append_output("exec-1", &m1).unwrap();
        daemon_doc.append_output("exec-1", &m2).unwrap();
        daemon_doc.append_output("exec-2", &m3).unwrap();

        let mut client_doc = RuntimeStateDoc::new_empty();
        let mut daemon_sync = sync::State::new();
        let mut client_sync = sync::State::new();

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

        let client_state = client_doc.read_state();
        assert_eq!(client_state.executions.len(), 2);
        assert_eq!(client_state.executions["exec-1"].outputs, vec![m1, m2]);
        assert_eq!(client_state.executions["exec-2"].outputs, vec![m3]);
    }

    #[test]
    fn test_fork_and_merge_outputs() {
        let mut doc = RuntimeStateDoc::new();
        doc.create_execution("exec-1", "cell-1");
        let m1 = test_stream("h1");
        let m2 = test_stream("h2");
        doc.append_output("exec-1", &m1).unwrap();

        let mut fork = doc.fork();
        fork.set_actor("runtimed:state:iopub");
        fork.append_output("exec-1", &m2).unwrap();

        doc.merge(&mut fork).unwrap();
        assert_eq!(doc.get_outputs("exec-1"), vec![m1, m2]);
    }

    #[test]
    fn test_get_outputs_nonexistent() {
        let doc = RuntimeStateDoc::new();
        // No execution entry → empty outputs
        assert!(doc.get_outputs("nope").is_empty());
    }

    // ── Comm tests ────────────────────────────────────────────────

    #[test]
    fn test_put_comm_roundtrip() {
        let mut doc = RuntimeStateDoc::new();
        let state = serde_json::json!({"value": 42});
        doc.put_comm(
            "comm-1",
            "jupyter.widget",
            "@jupyter-widgets/controls",
            "IntSliderModel",
            &state,
            0,
        );

        let entry = doc.get_comm("comm-1").unwrap();
        assert_eq!(entry.target_name, "jupyter.widget");
        assert_eq!(entry.model_module, "@jupyter-widgets/controls");
        assert_eq!(entry.model_name, "IntSliderModel");
        assert_eq!(entry.state, serde_json::json!({"value": 42}));
        assert!(entry.outputs.is_empty());
        assert_eq!(entry.seq, 0);
    }

    #[test]
    fn test_put_comm_overwrites() {
        let empty = serde_json::json!({});
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm("comm-1", "jupyter.widget", "mod-a", "ModelA", &empty, 0);
        doc.put_comm("comm-1", "jupyter.widget", "mod-b", "ModelB", &empty, 1);

        let entry = doc.get_comm("comm-1").unwrap();
        assert_eq!(entry.model_module, "mod-b");
        assert_eq!(entry.model_name, "ModelB");
        assert_eq!(entry.seq, 1);
    }

    #[test]
    fn test_remove_comm() {
        let empty = serde_json::json!({});
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm("comm-1", "jupyter.widget", "", "", &empty, 0);
        assert!(doc.remove_comm("comm-1"));
        assert!(doc.get_comm("comm-1").is_none());
    }

    #[test]
    fn test_remove_comm_nonexistent() {
        let mut doc = RuntimeStateDoc::new();
        assert!(!doc.remove_comm("nope"));
    }

    #[test]
    fn test_clear_comms() {
        let empty = serde_json::json!({});
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm("comm-1", "jupyter.widget", "", "", &empty, 0);
        doc.put_comm("comm-2", "jupyter.widget", "", "", &empty, 1);
        assert!(doc.clear_comms());
        assert!(doc.get_comm("comm-1").is_none());
        assert!(doc.get_comm("comm-2").is_none());
    }

    #[test]
    fn test_clear_comms_empty() {
        let mut doc = RuntimeStateDoc::new();
        assert!(!doc.clear_comms());
    }

    #[test]
    fn test_comms_in_read_state() {
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm(
            "comm-1",
            "jupyter.widget",
            "mod",
            "Slider",
            &serde_json::json!({"v": 1}),
            0,
        );
        doc.put_comm(
            "comm-2",
            "jupyter.widget",
            "mod",
            "Button",
            &serde_json::json!({"v": 2}),
            1,
        );

        let state = doc.read_state();
        assert_eq!(state.comms.len(), 2);
        assert_eq!(state.comms["comm-1"].model_name, "Slider");
        assert_eq!(state.comms["comm-2"].model_name, "Button");
    }

    #[test]
    fn test_fork_and_merge_comms() {
        let empty = serde_json::json!({});
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm("comm-1", "jupyter.widget", "", "", &empty, 0);

        let mut fork = doc.fork();
        fork.set_actor("runtimed:state:comms");
        fork.put_comm("comm-2", "jupyter.widget", "", "New", &empty, 1);

        doc.merge(&mut fork).unwrap();
        assert!(doc.get_comm("comm-1").is_some());
        assert_eq!(doc.get_comm("comm-2").unwrap().model_name, "New");
    }

    #[test]
    fn test_comm_output_append_and_clear() {
        let empty = serde_json::json!({});
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm("comm-1", "jupyter.widget", "", "OutputModel", &empty, 0);

        let m_a = test_display("hash-a");
        let m_b = test_display("hash-b");
        assert!(doc.append_comm_output("comm-1", &m_a));
        assert!(doc.append_comm_output("comm-1", &m_b));
        assert_eq!(doc.get_comm("comm-1").unwrap().outputs, vec![m_a, m_b]);

        assert!(doc.clear_comm_outputs("comm-1"));
        assert!(doc.get_comm("comm-1").unwrap().outputs.is_empty());

        // Clearing already-empty returns false
        assert!(!doc.clear_comm_outputs("comm-1"));
    }

    #[test]
    fn test_comm_output_nonexistent() {
        let mut doc = RuntimeStateDoc::new();
        assert!(!doc.append_comm_output("nope", &test_stream("hash")));
        assert!(!doc.clear_comm_outputs("nope"));
    }

    #[test]
    fn test_new_doc_has_empty_comms() {
        let doc = RuntimeStateDoc::new();
        assert!(doc.read_state().comms.is_empty());
    }

    #[test]
    fn test_set_comm_state_property() {
        let mut doc = RuntimeStateDoc::new();
        doc.put_comm(
            "comm-1",
            "jupyter.widget",
            "",
            "IntSliderModel",
            &serde_json::json!({"value": 50, "min": 0, "max": 100}),
            0,
        );

        // Set a single property
        assert!(doc.set_comm_state_property("comm-1", "value", &serde_json::json!(75)));

        let entry = doc.get_comm("comm-1").unwrap();
        assert_eq!(entry.state["value"], 75);
        // Other properties preserved
        assert_eq!(entry.state["min"], 0);
        assert_eq!(entry.state["max"], 100);
    }

    #[test]
    fn test_set_comm_state_property_nonexistent() {
        let mut doc = RuntimeStateDoc::new();
        assert!(!doc.set_comm_state_property("nope", "value", &serde_json::json!(42)));
    }

    #[test]
    fn test_native_state_roundtrip() {
        let mut doc = RuntimeStateDoc::new();
        let state = serde_json::json!({
            "value": 42,
            "description": "Speed:",
            "disabled": false,
            "layout": "IPY_MODEL_abc123",
            "nested": {"a": [1, 2, 3]}
        });
        doc.put_comm("comm-1", "jupyter.widget", "", "", &state, 0);

        let entry = doc.get_comm("comm-1").unwrap();
        assert_eq!(entry.state, state);
    }

    // ── Output diff tests ─────────────────────────────────────────

    #[test]
    fn test_diff_new_output_detected() {
        let prev = HashMap::new();
        let mut execs = HashMap::new();
        execs.insert(
            "exec-1".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "done".to_string(),
                execution_count: Some(1),
                success: Some(true),
                outputs: vec![test_stream("hash1")],
                source: None,
                seq: None,
            },
        );
        let (changed, _) = diff_execution_outputs(&prev, &execs);
        assert_eq!(changed, vec!["cell-1"]);
    }

    #[test]
    fn test_diff_empty_to_empty_no_change() {
        let prev = HashMap::new();
        let mut execs = HashMap::new();
        execs.insert(
            "exec-1".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "running".to_string(),
                execution_count: None,
                success: None,
                outputs: vec![],
                source: None,
                seq: None,
            },
        );
        let (changed, _) = diff_execution_outputs(&prev, &execs);
        assert!(changed.is_empty());
    }

    #[test]
    fn test_diff_output_cleared_detected() {
        let mut prev = HashMap::new();
        prev.insert("exec-1".to_string(), vec![test_stream("hash1")]);
        let mut execs = HashMap::new();
        execs.insert(
            "exec-1".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "done".to_string(),
                execution_count: Some(1),
                success: Some(true),
                outputs: vec![],
                source: None,
                seq: None,
            },
        );
        let (changed, _) = diff_execution_outputs(&prev, &execs);
        assert_eq!(changed, vec!["cell-1"]);
    }

    /// The critical edge case: after outputs are cleared, the snapshot
    /// must retain the empty outputs so the NEXT diff comparing
    /// [] vs ["new_hash"] correctly detects the change.
    #[test]
    fn test_diff_re_execution_output_after_clear() {
        // Stage 1: execution has output
        let prev = HashMap::new();
        let mut execs = HashMap::new();
        execs.insert(
            "exec-1".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "done".to_string(),
                execution_count: Some(1),
                success: Some(true),
                outputs: vec![test_stream("hash1")],
                source: None,
                seq: None,
            },
        );
        let (changed, snapshot) = diff_execution_outputs(&prev, &execs);
        assert_eq!(changed, vec!["cell-1"]);

        // Stage 2: outputs cleared (pre-execute)
        execs.get_mut("exec-1").unwrap().outputs = vec![];
        let (changed, snapshot) = diff_execution_outputs(&snapshot, &execs);
        assert_eq!(changed, vec!["cell-1"], "clear should be detected");

        // Stage 3: new execution with empty outputs (just created)
        execs.insert(
            "exec-2".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "running".to_string(),
                execution_count: None,
                success: None,
                outputs: vec![],
                source: None,
                seq: None,
            },
        );
        let (changed, snapshot) = diff_execution_outputs(&snapshot, &execs);
        assert!(changed.is_empty(), "no output change yet");

        // Stage 4: new output arrives on exec-2
        execs.get_mut("exec-2").unwrap().outputs = vec![test_stream("hash2")];
        let (changed, _) = diff_execution_outputs(&snapshot, &execs);
        assert_eq!(
            changed,
            vec!["cell-1"],
            "new output after clear must be detected"
        );
    }

    /// Verify snapshot retains empty outputs (the original bug was
    /// filtering them out, which broke subsequent diffs).
    #[test]
    fn test_diff_snapshot_retains_empty_outputs() {
        let mut prev = HashMap::new();
        prev.insert("exec-1".to_string(), vec![test_stream("hash1")]);

        let mut execs = HashMap::new();
        execs.insert(
            "exec-1".to_string(),
            ExecutionState {
                cell_id: "cell-1".to_string(),
                status: "done".to_string(),
                execution_count: Some(1),
                success: Some(true),
                outputs: vec![],
                source: None,
                seq: None,
            },
        );

        let (_, snapshot) = diff_execution_outputs(&prev, &execs);
        assert!(
            snapshot.contains_key("exec-1"),
            "empty outputs must be retained in snapshot"
        );
        assert!(snapshot["exec-1"].is_empty());
    }

    #[test]
    fn test_merge_comm_state_delta_skips_same_value() {
        let mut doc = RuntimeStateDoc::new();
        let state = serde_json::json!({"value": 42, "label": "hello"});
        doc.put_comm("w1", "jupyter.widget", "", "", &state, 0);

        // Same values → no change
        let delta = serde_json::json!({"value": 42, "label": "hello"});
        assert!(!doc.merge_comm_state_delta("w1", &delta));
    }

    #[test]
    fn test_merge_comm_state_delta_writes_changed_value() {
        let mut doc = RuntimeStateDoc::new();
        let state = serde_json::json!({"value": 42, "label": "hello"});
        doc.put_comm("w1", "jupyter.widget", "", "", &state, 0);

        // Different value → change
        let delta = serde_json::json!({"value": 99});
        assert!(doc.merge_comm_state_delta("w1", &delta));
        let updated = doc.read_state();
        let w1 = &updated.comms["w1"];
        assert_eq!(w1.state["value"], 99);
        // Unchanged key preserved
        assert_eq!(w1.state["label"], "hello");
    }

    #[test]
    fn test_merge_comm_state_delta_nonexistent_comm() {
        let mut doc = RuntimeStateDoc::new();
        let delta = serde_json::json!({"value": 1});
        assert!(!doc.merge_comm_state_delta("nonexistent", &delta));
    }

    #[test]
    fn test_merge_comm_state_delta_writes_objects_unconditionally() {
        let mut doc = RuntimeStateDoc::new();
        let state = serde_json::json!({"nested": {"a": 1}});
        doc.put_comm("w1", "jupyter.widget", "", "", &state, 0);

        // Object values are always written (no deep comparison)
        let delta = serde_json::json!({"nested": {"a": 1}});
        assert!(doc.merge_comm_state_delta("w1", &delta));
    }
}
