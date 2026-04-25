//! Room-based notebook synchronization server.
//!
//! Each open notebook gets a "room" in the daemon. Multiple windows editing
//! the same notebook sync through the room's canonical Automerge document.
//!
//! Follows the same sync protocol pattern as `sync_server.rs` (settings sync)
//! but with per-notebook state managed through rooms.
//!
//! ## Room lifecycle
//!
//! 1. First window opens notebook -> daemon creates room, loads persisted doc
//! 2. Client exchanges Automerge sync messages with the room
//! 3. Additional windows join the same room
//! 4. Changes from any peer broadcast to all others in the room
//! 5. When the last peer disconnects, the room is evicted from memory
//!    (any pending doc bytes are flushed to disk via debounced persistence)
//! 6. Documents persist to `~/.cache/runt/notebook-docs/{hash}.automerge`
//!
//! ## Phase 8: Daemon-owned kernel execution
//!
//! Each room can have an optional kernel. When a kernel is launched:
//! - Execute requests flow through the daemon
//! - Daemon tracks msg_id -> cell_id mapping
//! - Outputs are broadcast to all connected windows
//! - Multiple windows share the same kernel

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use automerge::sync;
use notify_debouncer_mini::DebounceEventResult;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::blob_store::BlobStore;
use crate::connection::{self, NotebookFrameType};
use crate::markdown_assets::resolve_markdown_assets;
use crate::notebook_doc::{CellSnapshot, NotebookDoc};
use crate::notebook_metadata::NotebookMetadataSnapshot;
use crate::output_prep::{DenoLaunchedConfig, LaunchedEnvConfig};
use crate::paths::notebook_doc_filename;
use crate::protocol::{EnvSyncDiff, NotebookBroadcast, NotebookRequest, NotebookResponse};
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};
use notebook_doc::diff::diff_metadata_touched;
use notebook_doc::presence::{self, PresenceState};
use runtime_doc::RuntimeStateDoc;

mod catalog;
mod load;
mod metadata;
mod path_index;
mod peer;
mod persist;
mod room;
mod runtime_bridge;
#[cfg(test)]
mod tests;

pub(crate) use catalog::*;
pub(crate) use load::*;
pub(crate) use metadata::*;
pub use path_index::{PathIndex, PathIndexError};
pub(crate) use peer::*;
pub(crate) use persist::*;
pub(crate) use room::*;
pub(crate) use runtime_bridge::*;

/// Global shutdown trigger registered by the daemon at startup.
///
/// Used by `spawn_supervised` callbacks in debouncers (autosave, persist) that
/// don't have `Arc<Daemon>` in scope. The daemon calls `register_shutdown_trigger`
/// once; on_panic handlers call `trigger_global_shutdown` to signal data-loss risk.
static SHUTDOWN_TRIGGER: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

pub fn register_shutdown_trigger(trigger: Arc<dyn Fn() + Send + Sync>) {
    let _ = SHUTDOWN_TRIGGER.set(trigger);
}

fn trigger_global_shutdown() {
    if let Some(trigger) = SHUTDOWN_TRIGGER.get() {
        (trigger)();
    }
}

/// Capacity for the per-room kernel broadcast channel. Sized to absorb bursts
/// of output messages (e.g. fast-printing cells) so slower peers trigger a
/// full doc sync ("peer lagged") rather than losing messages.
const KERNEL_BROADCAST_CAPACITY: usize = 256;

/// Compaction threshold for RuntimeStateDoc initial sync messages.
/// If the encoded message exceeds this, compact before sending. Leaves
/// 20 MiB headroom under the 100 MiB frame limit.
const STATE_SYNC_COMPACT_THRESHOLD: usize = 80 * 1024 * 1024;

/// Catch panics from automerge internal operations.
///
/// Automerge 0.7.4 (and 0.8.0) has a known bug where the change collector
/// panics with `MissingOps` when internal op-set indices become inconsistent
/// (see `op_set2/change/collector.rs:761`). This affects `generate_sync_message`,
/// `fork_at`, `merge`, and `get_changes`.
///
/// After catching a panic, callers should call `rebuild_from_save()` on the
/// affected doc to round-trip save->load and rebuild clean internal indices.
pub(crate) fn catch_automerge_panic<T>(label: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(val) => Ok(val),
        Err(payload) => {
            let msg = crate::task_supervisor::panic_payload_to_string(payload);
            Err(format!(
                "[{label}] automerge panicked (upstream bug, see automerge collector.rs MissingOps): {msg}"
            ))
        }
    }
}

/// A message sent through the runtime agent channel.
pub enum RuntimeAgentMessage {
    /// Fire-and-forget command - no response expected.
    Command(notebook_protocol::protocol::RuntimeAgentRequestEnvelope),
    /// Query requiring a sync response via correlation ID.
    Query(
        notebook_protocol::protocol::RuntimeAgentRequestEnvelope,
        tokio::sync::oneshot::Sender<notebook_protocol::protocol::RuntimeAgentResponse>,
    ),
}

/// Sender half of the runtime agent channel.
type RuntimeAgentRequestSender = tokio::sync::mpsc::Sender<RuntimeAgentMessage>;

fn runtime_agent_query_timeout(
    request: &notebook_protocol::protocol::RuntimeAgentRequest,
) -> std::time::Duration {
    use notebook_protocol::protocol::RuntimeAgentRequest;
    match request {
        RuntimeAgentRequest::Complete { .. } | RuntimeAgentRequest::GetHistory { .. } => {
            std::time::Duration::from_secs(10)
        }
        RuntimeAgentRequest::LaunchKernel { .. }
        | RuntimeAgentRequest::RestartKernel { .. }
        | RuntimeAgentRequest::SyncEnvironment(_) => std::time::Duration::from_secs(240),
        _ => std::time::Duration::from_secs(30),
    }
}
