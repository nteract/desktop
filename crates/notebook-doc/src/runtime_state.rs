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
//!   env/
//!     in_sync: bool
//!     added: List[Str]     (packages in metadata but not in kernel)
//!     removed: List[Str]   (packages in kernel but not in metadata)
//!     channels_changed: bool
//!     deno_changed: bool
//!   trust/
//!     status: Str          ("trusted" | "untrusted" | "signature_invalid" | "no_dependencies")
//!     needs_approval: bool
//!   last_saved: Str|null   (ISO timestamp of last save)
//! ```

use automerge::{
    sync, sync::SyncDoc, transaction::Transactable, ActorId, AutoCommit, AutomergeError, ObjType,
    ReadDoc, ScalarValue, Value, ROOT,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Trust state snapshot for the runtime state doc.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRuntimeState {
    /// "trusted", "untrusted", "signature_invalid", "no_dependencies"
    pub status: String,
    /// Whether the frontend should show the trust approval dialog
    pub needs_approval: bool,
}

/// Full runtime state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    pub kernel: KernelState,
    pub queue: QueueState,
    pub env: EnvState,
    pub trust: TrustRuntimeState,
    pub last_saved: Option<String>,
    /// Execution lifecycle entries keyed by execution_id.
    #[serde(default)]
    pub executions: HashMap<String, ExecutionState>,
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

        // trust/
        let trust = doc
            .put_object(&ROOT, "trust", ObjType::Map)
            .expect("scaffold trust");
        doc.put(&trust, "status", "no_dependencies")
            .expect("scaffold trust.status");
        doc.put(&trust, "needs_approval", false)
            .expect("scaffold trust.needs_approval");

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

        Some(ExecutionState {
            cell_id,
            status,
            execution_count,
            success,
        })
    }

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
        let trust = self.get_map("trust");

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

        RuntimeState {
            kernel: kernel_state,
            queue: queue_state,
            env: env_state,
            trust: trust_state,
            last_saved,
            executions,
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
}
