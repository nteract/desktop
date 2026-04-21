//! Error types for ipynb <-> NotebookDoc conversion.

use thiserror::Error;

/// Errors produced when converting between `.ipynb` JSON and `NotebookDoc`.
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// The input JSON does not have the minimum shape of an ipynb document
    /// (e.g. missing the top-level `cells` array).
    #[error("invalid ipynb: {0}")]
    InvalidIpynb(String),

    /// An Automerge mutation failed while building the `NotebookDoc`.
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
}

/// Type alias for persistence results.
pub type Result<T> = std::result::Result<T, PersistenceError>;
