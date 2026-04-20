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
