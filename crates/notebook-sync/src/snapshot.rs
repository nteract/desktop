//! Read-only notebook snapshot published via `tokio::sync::watch`.
//!
//! The `DocHandle` publishes a new snapshot after every document mutation
//! (local or remote). Readers access the latest state without acquiring
//! the document mutex — they just borrow from the watch channel.

use std::sync::Arc;

use notebook_doc::metadata::NotebookMetadataSnapshot;
use notebook_doc::CellSnapshot;

/// A point-in-time snapshot of the notebook document.
///
/// Published by the `DocHandle` after every mutation. Readers clone the
/// `Arc<Vec<CellSnapshot>>` cheaply — the cell data itself is shared.
///
/// This is the read side of the document. For writes, use `DocHandle::with_doc`.
#[derive(Clone, Debug)]
pub struct NotebookSnapshot {
    /// All cells in document order (sorted by fractional index position).
    pub cells: Arc<Vec<CellSnapshot>>,

    /// Parsed notebook metadata (kernelspec, language_info, runt config).
    /// `None` if the document has no metadata yet.
    pub notebook_metadata: Option<NotebookMetadataSnapshot>,
}

impl NotebookSnapshot {
    /// Create a snapshot from the current document state.
    pub fn from_doc(doc: &automerge::AutoCommit) -> Self {
        use notebook_doc::{get_cells_from_doc, get_metadata_snapshot_from_doc};

        Self {
            cells: Arc::new(get_cells_from_doc(doc)),
            notebook_metadata: get_metadata_snapshot_from_doc(doc),
        }
    }

    /// Create an empty snapshot (no cells, no metadata).
    pub fn empty() -> Self {
        Self {
            cells: Arc::new(Vec::new()),
            notebook_metadata: None,
        }
    }

    /// Get cells as a slice.
    pub fn cells(&self) -> &[CellSnapshot] {
        &self.cells
    }

    /// Get a cell by ID.
    pub fn get_cell(&self, cell_id: &str) -> Option<&CellSnapshot> {
        self.cells.iter().find(|c| c.id == cell_id)
    }

    /// Get the number of cells.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Get the typed notebook metadata snapshot.
    pub fn notebook_metadata(&self) -> Option<&NotebookMetadataSnapshot> {
        self.notebook_metadata.as_ref()
    }
}

impl Default for NotebookSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}
