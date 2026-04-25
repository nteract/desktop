//! Notebook session state management.

use notebook_sync::handle::DocHandle;
use notebook_sync::BroadcastReceiver;

/// An active notebook session connected via the daemon.
#[allow(dead_code)] // Fields used by tool handlers as more tools are ported
pub struct NotebookSession {
    /// The Automerge document handle for this notebook.
    pub handle: DocHandle,
    /// The notebook ID (always a UUID).
    pub notebook_id: String,
    /// The file path for file-backed notebooks (opened via `open_notebook`).
    /// `None` for ephemeral notebooks created via `create_notebook`.
    pub notebook_path: Option<String>,
    /// Broadcast receiver for daemon events (execution done, outputs, etc.)
    pub broadcast_rx: BroadcastReceiver,
}

/// Why the session was dropped. Recorded when the session transitions from
/// `Some` → `None` so the "no active session" error can tell agents *why*
/// and *how to recover* instead of a generic message.
#[derive(Debug, Clone)]
pub enum SessionDropReason {
    /// Room was evicted by the daemon (idle timeout).
    Evicted,
    /// Agent switched to a different notebook (normal, not an error).
    Switched,
    /// Daemon connection was lost and rejoin failed.
    Disconnected,
}

impl std::fmt::Display for SessionDropReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evicted => write!(f, "notebook was evicted (idle timeout)"),
            Self::Switched => write!(f, "switched to a different notebook"),
            Self::Disconnected => write!(f, "daemon connection lost"),
        }
    }
}

/// Context from the most recently dropped session — enough for the error
/// message to tell the agent what happened and how to recover.
#[derive(Debug, Clone)]
pub struct SessionDropInfo {
    pub reason: SessionDropReason,
    pub notebook_id: String,
    pub notebook_path: Option<String>,
}
