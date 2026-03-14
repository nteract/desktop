//! Error types for notebook-sync operations.

/// Errors that can occur during notebook sync operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The document mutex was poisoned (a thread panicked while holding it).
    #[error("Document lock poisoned")]
    LockPoisoned,

    /// An Automerge operation failed.
    #[error("Automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),

    /// A network I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The sync task has stopped (channels closed).
    #[error("Disconnected from sync task")]
    Disconnected,

    /// A daemon protocol error.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Connection timed out.
    #[error("Connection timed out")]
    Timeout,

    /// A cell was not found in the document.
    #[error("Cell not found: {0}")]
    CellNotFound(String),

    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<serde_json::Error> for SyncError {
    fn from(e: serde_json::Error) -> Self {
        SyncError::Serialization(e.to_string())
    }
}
