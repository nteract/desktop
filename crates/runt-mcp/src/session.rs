//! Notebook session state management.

use notebook_sync::handle::DocHandle;
use notebook_sync::BroadcastReceiver;

/// An active notebook session connected via the daemon.
#[allow(dead_code)] // Fields used by tool handlers as more tools are ported
pub struct NotebookSession {
    /// The Automerge document handle for this notebook.
    pub handle: DocHandle,
    /// The notebook ID (file path or UUID).
    pub notebook_id: String,
    /// Broadcast receiver for daemon events (execution done, outputs, etc.)
    pub broadcast_rx: BroadcastReceiver,
}
