//! Automerge-backed notebook document for cross-window sync.
//!
//! Wraps an Automerge `AutoCommit` document with typed accessors for
//! notebook cells, outputs, and metadata. The daemon holds the canonical
//! copy in a "room"; each connected notebook window holds a local replica
//! that syncs via the Automerge sync protocol.
//!
//! ## Document schema
//!
//! ```text
//! ROOT/
//!   notebook_id: Str
//!   cells/                        ← List of Map
//!     [i]/
//!       id: Str                   ← cell UUID
//!       cell_type: Str            ← "code" | "markdown" | "raw"
//!       source: Text              ← Automerge Text CRDT (character-level merging)
//!       execution_count: Str      ← JSON-encoded i32 or "null"
//!       outputs/                  ← List of Str
//!         [j]: Str                ← JSON-encoded Jupyter output (Phase 5: manifest hash)
//!   metadata/                     ← Map
//!     runtime: Str
//!     notebook_metadata: Str      ← JSON-encoded NotebookMetadataSnapshot
//! ```

use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, AutomergeError, ObjId, ObjType, ReadDoc};
use serde::{Deserialize, Serialize};

/// Snapshot of a single cell's state, suitable for serialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CellSnapshot {
    pub id: String,
    /// "code", "markdown", or "raw"
    pub cell_type: String,
    pub source: String,
    /// JSON-encoded execution count: a number string like "5" or "null"
    pub execution_count: String,
    /// JSON-encoded Jupyter output objects (will become manifest hashes in Phase 5)
    pub outputs: Vec<String>,
}

/// Wrapper around an Automerge document storing a notebook.
pub struct NotebookDoc {
    doc: AutoCommit,
}

impl NotebookDoc {
    /// Create a new empty notebook document with the given ID.
    pub fn new(notebook_id: &str) -> Self {
        let mut doc = AutoCommit::new();

        let _ = doc.put(automerge::ROOT, "notebook_id", notebook_id);

        // cells: empty List
        let _ = doc.put_object(automerge::ROOT, "cells", ObjType::List);

        // metadata: Map with default runtime
        if let Ok(meta_id) = doc.put_object(automerge::ROOT, "metadata", ObjType::Map) {
            let _ = doc.put(&meta_id, "runtime", "python");
        }

        Self { doc }
    }

    /// Load a notebook document from saved bytes.
    pub fn load(data: &[u8]) -> Result<Self, AutomergeError> {
        let doc = AutoCommit::load(data)?;
        Ok(Self { doc })
    }

    /// Serialize the document to bytes.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    // ── Notebook ID ─────────────────────────────────────────────────

    /// Read the notebook ID from the document.
    #[allow(dead_code)]
    pub fn notebook_id(&self) -> Option<String> {
        read_str(&self.doc, automerge::ROOT, "notebook_id")
    }

    // ── Cell CRUD ───────────────────────────────────────────────────

    /// Number of cells in the notebook.
    pub fn cell_count(&self) -> usize {
        match self.cells_list_id() {
            Some(id) => self.doc.length(&id),
            None => 0,
        }
    }

    /// Get all cells as snapshots, in order.
    pub fn get_cells(&self) -> Vec<CellSnapshot> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return vec![],
        };
        let len = self.doc.length(&cells_id);
        (0..len)
            .filter_map(|i| {
                let cell_obj = self.cell_at_index(&cells_id, i)?;
                self.read_cell(&cell_obj)
            })
            .collect()
    }

    /// Get a single cell by ID.
    #[allow(dead_code)]
    pub fn get_cell(&self, cell_id: &str) -> Option<CellSnapshot> {
        let cells_id = self.cells_list_id()?;
        let idx = self.find_cell_index(&cells_id, cell_id)?;
        let cell_obj = self.cell_at_index(&cells_id, idx)?;
        self.read_cell(&cell_obj)
    }

    /// Insert a new cell at the given index.
    ///
    /// Returns `Ok(())` on success. The cell starts with empty source and no outputs.
    pub fn add_cell(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), AutomergeError> {
        let cells_id = self
            .cells_list_id()
            .ok_or_else(|| AutomergeError::InvalidObjId("cells list not found".into()))?;

        // Clamp index to list length
        let len = self.doc.length(&cells_id);
        let index = index.min(len);

        let cell_map = self.doc.insert_object(&cells_id, index, ObjType::Map)?;
        self.doc.put(&cell_map, "id", cell_id)?;
        self.doc.put(&cell_map, "cell_type", cell_type)?;
        self.doc.put_object(&cell_map, "source", ObjType::Text)?;
        self.doc.put(&cell_map, "execution_count", "null")?;
        self.doc.put_object(&cell_map, "outputs", ObjType::List)?;
        Ok(())
    }

    /// Delete a cell by ID. Returns `true` if the cell was found and deleted.
    pub fn delete_cell(&mut self, cell_id: &str) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        match self.find_cell_index(&cells_id, cell_id) {
            Some(idx) => {
                self.doc.delete(&cells_id, idx)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    // ── Source editing ───────────────────────────────────────────────

    /// Replace a cell's source text.
    ///
    /// Uses `update_text` which performs a Myers diff internally, producing
    /// minimal CRDT operations for better concurrent edit merging.
    pub fn update_source(
        &mut self,
        cell_id: &str,
        new_source: &str,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Ok(false),
        };
        let source_id = match self.text_id(&cell_obj, "source") {
            Some(id) => id,
            None => return Ok(false),
        };

        self.doc.update_text(&source_id, new_source)?;
        Ok(true)
    }

    /// Append text to a cell's source without diffing.
    ///
    /// Unlike `update_source` which replaces the entire text (using Myers diff
    /// internally), this directly inserts characters at the end of the source
    /// Text CRDT. This is ideal for streaming/agentic use cases where an
    /// external process is appending tokens incrementally.
    pub fn append_source(&mut self, cell_id: &str, text: &str) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Ok(false),
        };
        let source_id = match self.text_id(&cell_obj, "source") {
            Some(id) => id,
            None => return Ok(false),
        };

        let len = self.doc.text(&source_id)?.len();
        self.doc.splice_text(&source_id, len, 0, text)?;
        Ok(true)
    }

    // ── Output management ───────────────────────────────────────────

    /// Replace all outputs for a cell.
    #[allow(dead_code)]
    pub fn set_outputs(
        &mut self,
        cell_id: &str,
        outputs: &[String],
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Ok(false),
        };

        // Delete existing outputs and create fresh list
        let _ = self.doc.delete(&cell_obj, "outputs");
        let list_id = self.doc.put_object(&cell_obj, "outputs", ObjType::List)?;
        for (i, output) in outputs.iter().enumerate() {
            self.doc.insert(&list_id, i, output.as_str())?;
        }
        Ok(true)
    }

    /// Append a single output to a cell's output list.
    #[allow(dead_code)]
    pub fn append_output(&mut self, cell_id: &str, output: &str) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Ok(false),
        };
        let outputs_id = match self.list_id(&cell_obj, "outputs") {
            Some(id) => id,
            None => return Ok(false),
        };

        let len = self.doc.length(&outputs_id);
        self.doc.insert(&outputs_id, len, output)?;
        Ok(true)
    }

    /// Clear all outputs from a cell.
    #[allow(dead_code)]
    pub fn clear_outputs(&mut self, cell_id: &str) -> Result<bool, AutomergeError> {
        self.set_outputs(cell_id, &[])
    }

    // ── Metadata ────────────────────────────────────────────────────

    /// Read a metadata value.
    pub fn get_metadata(&self, key: &str) -> Option<String> {
        let meta_id = self.metadata_map_id()?;
        read_str(&self.doc, meta_id, key)
    }

    /// Set a metadata value.
    pub fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), AutomergeError> {
        let meta_id = match self.metadata_map_id() {
            Some(id) => id,
            None => {
                // Create metadata map if missing
                let id = self
                    .doc
                    .put_object(automerge::ROOT, "metadata", ObjType::Map)?;
                self.doc.put(&id, key, value)?;
                return Ok(());
            }
        };
        self.doc.put(&meta_id, key, value)?;
        Ok(())
    }

    // ── Sync protocol ───────────────────────────────────────────────

    /// Generate a sync message to send to a peer.
    pub fn generate_sync_message(&mut self, peer_state: &mut sync::State) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(peer_state)
    }

    /// Receive and apply a sync message from a peer.
    pub fn receive_sync_message(
        &mut self,
        peer_state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        self.doc.sync().receive_sync_message(peer_state, message)
    }

    // ── Internal helpers ────────────────────────────────────────────

    fn cells_list_id(&self) -> Option<ObjId> {
        self.doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::List) => Some(id),
                _ => None,
            })
    }

    fn metadata_map_id(&self) -> Option<ObjId> {
        self.doc
            .get(automerge::ROOT, "metadata")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn cell_at_index(&self, cells_id: &ObjId, index: usize) -> Option<ObjId> {
        self.doc
            .get(cells_id, index)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn find_cell_index(&self, cells_id: &ObjId, cell_id: &str) -> Option<usize> {
        let len = self.doc.length(cells_id);
        for i in 0..len {
            if let Some(cell_obj) = self.cell_at_index(cells_id, i) {
                if read_str(&self.doc, &cell_obj, "id").as_deref() == Some(cell_id) {
                    return Some(i);
                }
            }
        }
        None
    }

    fn text_id(&self, parent: &ObjId, key: &str) -> Option<ObjId> {
        self.doc
            .get(parent, key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Text) => Some(id),
                _ => None,
            })
    }

    fn list_id(&self, parent: &ObjId, key: &str) -> Option<ObjId> {
        self.doc
            .get(parent, key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::List) => Some(id),
                _ => None,
            })
    }

    fn read_cell(&self, cell_obj: &ObjId) -> Option<CellSnapshot> {
        let id = read_str(&self.doc, cell_obj, "id")?;
        let cell_type = read_str(&self.doc, cell_obj, "cell_type").unwrap_or_default();
        let execution_count =
            read_str(&self.doc, cell_obj, "execution_count").unwrap_or_else(|| "null".to_string());

        // Read source from Text CRDT
        let source = self
            .text_id(cell_obj, "source")
            .and_then(|text_id| self.doc.text(&text_id).ok())
            .unwrap_or_default();

        // Read outputs list
        let outputs = match self.list_id(cell_obj, "outputs") {
            Some(list_id) => {
                let len = self.doc.length(&list_id);
                (0..len)
                    .filter_map(|i| read_str(&self.doc, &list_id, i))
                    .collect()
            }
            None => vec![],
        };

        Some(CellSnapshot {
            id,
            cell_type,
            source,
            execution_count,
            outputs,
        })
    }
}

// ── Free helpers ─────────────────────────────────────────────────────

/// Read a scalar string from any Automerge object by key.
fn read_str<O: AsRef<automerge::ObjId>, P: Into<automerge::Prop>>(
    doc: &AutoCommit,
    obj: O,
    prop: P,
) -> Option<String> {
    doc.get(obj, prop)
        .ok()
        .flatten()
        .and_then(|(value, _)| match value {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        })
}

/// Read a metadata value from a raw `AutoCommit` document.
///
/// This is the free-function counterpart of `NotebookDoc::get_metadata`,
/// for use by the sync client which holds a raw `AutoCommit` instead of
/// a `NotebookDoc`.
#[allow(dead_code)]
pub fn get_metadata_from_doc(doc: &AutoCommit, key: &str) -> Option<String> {
    let meta_id = doc
        .get(automerge::ROOT, "metadata")
        .ok()
        .flatten()
        .and_then(|(value, id)| match value {
            automerge::Value::Object(ObjType::Map) => Some(id),
            _ => None,
        })?;
    read_str(doc, meta_id, key)
}

/// Set a metadata value in a raw `AutoCommit` document.
///
/// Creates the metadata map if it doesn't exist. This is the free-function
/// counterpart of `NotebookDoc::set_metadata`.
#[allow(dead_code)]
pub fn set_metadata_in_doc(
    doc: &mut AutoCommit,
    key: &str,
    value: &str,
) -> Result<(), AutomergeError> {
    let meta_id = doc
        .get(automerge::ROOT, "metadata")
        .ok()
        .flatten()
        .and_then(|(v, id)| match v {
            automerge::Value::Object(ObjType::Map) => Some(id),
            _ => None,
        });

    let meta_id = match meta_id {
        Some(id) => id,
        None => doc.put_object(automerge::ROOT, "metadata", ObjType::Map)?,
    };

    doc.put(&meta_id, key, value)?;
    Ok(())
}

/// Read cells from a raw AutoCommit document (used by the sync client).
#[allow(dead_code)]
pub fn get_cells_from_doc(doc: &AutoCommit) -> Vec<CellSnapshot> {
    let cells_id = match doc.get(automerge::ROOT, "cells").ok().flatten() {
        Some((automerge::Value::Object(ObjType::List), id)) => id,
        _ => return vec![],
    };

    let len = doc.length(&cells_id);
    (0..len)
        .filter_map(|i| {
            let cell_obj = match doc.get(&cells_id, i).ok().flatten() {
                Some((automerge::Value::Object(ObjType::Map), id)) => id,
                _ => return None,
            };

            let id = read_str(doc, &cell_obj, "id")?;
            let cell_type = read_str(doc, &cell_obj, "cell_type").unwrap_or_default();
            let execution_count =
                read_str(doc, &cell_obj, "execution_count").unwrap_or_else(|| "null".to_string());

            let source = doc
                .get(&cell_obj, "source")
                .ok()
                .flatten()
                .and_then(|(value, text_id)| match value {
                    automerge::Value::Object(ObjType::Text) => doc.text(&text_id).ok(),
                    _ => None,
                })
                .unwrap_or_default();

            let outputs = match doc.get(&cell_obj, "outputs").ok().flatten() {
                Some((automerge::Value::Object(ObjType::List), list_id)) => {
                    let len = doc.length(&list_id);
                    (0..len)
                        .filter_map(|j| read_str(doc, &list_id, j))
                        .collect()
                }
                _ => vec![],
            };

            Some(CellSnapshot {
                id,
                cell_type,
                source,
                execution_count,
                outputs,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_has_empty_cells() {
        let doc = NotebookDoc::new("test-notebook");
        assert_eq!(doc.notebook_id(), Some("test-notebook".to_string()));
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(doc.get_cells(), vec![]);
        assert_eq!(doc.get_metadata("runtime"), Some("python".to_string()));
    }

    #[test]
    fn test_add_and_get_cell() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();

        assert_eq!(doc.cell_count(), 1);
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.id, "cell-1");
        assert_eq!(cell.cell_type, "code");
        assert_eq!(cell.source, "");
        assert_eq!(cell.execution_count, "null");
        assert!(cell.outputs.is_empty());
    }

    #[test]
    fn test_add_multiple_cells_ordering() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "first", "code").unwrap();
        doc.add_cell(1, "second", "markdown").unwrap();
        doc.add_cell(1, "middle", "code").unwrap(); // insert between first and second

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "first");
        assert_eq!(cells[1].id, "middle");
        assert_eq!(cells[2].id, "second");
    }

    #[test]
    fn test_add_cell_clamps_index() {
        let mut doc = NotebookDoc::new("nb1");
        // Index 100 on empty list should work (clamped to 0)
        doc.add_cell(100, "cell-1", "code").unwrap();
        assert_eq!(doc.cell_count(), 1);
        assert_eq!(doc.get_cells()[0].id, "cell-1");
    }

    #[test]
    fn test_delete_cell() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.add_cell(1, "cell-2", "markdown").unwrap();

        let deleted = doc.delete_cell("cell-1").unwrap();
        assert!(deleted);
        assert_eq!(doc.cell_count(), 1);
        assert_eq!(doc.get_cells()[0].id, "cell-2");
    }

    #[test]
    fn test_delete_nonexistent_cell() {
        let mut doc = NotebookDoc::new("nb1");
        let deleted = doc.delete_cell("nope").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_update_source() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();

        doc.update_source("cell-1", "print('hello')").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.source, "print('hello')");

        // Update again
        doc.update_source("cell-1", "print('world')").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.source, "print('world')");
    }

    #[test]
    fn test_update_source_empty() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "some code").unwrap();
        doc.update_source("cell-1", "").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.source, "");
    }

    #[test]
    fn test_update_source_nonexistent_cell() {
        let mut doc = NotebookDoc::new("nb1");
        let result = doc.update_source("nope", "code").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_set_outputs() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();

        let outputs = vec![
            r#"{"output_type":"stream","name":"stdout","text":"hello\n"}"#.to_string(),
            r#"{"output_type":"execute_result","data":{"text/plain":"42"}}"#.to_string(),
        ];
        doc.set_outputs("cell-1", &outputs).unwrap();

        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.outputs, outputs);
    }

    #[test]
    fn test_append_output() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();

        doc.append_output("cell-1", r#"{"output_type":"stream"}"#)
            .unwrap();
        doc.append_output("cell-1", r#"{"output_type":"display_data"}"#)
            .unwrap();

        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.outputs.len(), 2);
        assert!(cell.outputs[0].contains("stream"));
        assert!(cell.outputs[1].contains("display_data"));
    }

    #[test]
    fn test_clear_outputs() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.append_output("cell-1", "output1").unwrap();
        doc.append_output("cell-1", "output2").unwrap();

        doc.clear_outputs("cell-1").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert!(cell.outputs.is_empty());
    }

    #[test]
    fn test_metadata() {
        let mut doc = NotebookDoc::new("nb1");
        assert_eq!(doc.get_metadata("runtime"), Some("python".to_string()));

        doc.set_metadata("runtime", "deno").unwrap();
        assert_eq!(doc.get_metadata("runtime"), Some("deno".to_string()));

        doc.set_metadata("custom_key", "custom_value").unwrap();
        assert_eq!(
            doc.get_metadata("custom_key"),
            Some("custom_value".to_string())
        );
    }

    #[test]
    fn test_save_and_load() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 42").unwrap();
        doc.append_output("cell-1", r#"{"output_type":"execute_result"}"#)
            .unwrap();
        doc.add_cell(1, "cell-2", "markdown").unwrap();
        doc.update_source("cell-2", "# Hello").unwrap();

        let bytes = doc.save();
        let loaded = NotebookDoc::load(&bytes).unwrap();

        assert_eq!(loaded.notebook_id(), Some("nb1".to_string()));
        let cells = loaded.get_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].source, "x = 42");
        assert_eq!(cells[0].outputs.len(), 1);
        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].source, "# Hello");
    }

    #[test]
    fn test_sync_between_two_docs() {
        // Server creates a notebook with cells
        let mut server = NotebookDoc::new("sync-test");
        server.add_cell(0, "cell-1", "code").unwrap();
        server.update_source("cell-1", "import numpy").unwrap();
        server
            .append_output("cell-1", r#"{"output_type":"stream"}"#)
            .unwrap();

        // Client starts with an empty doc (like a new window joining)
        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };

        let mut server_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Exchange sync messages until convergence
        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        // Client should now have the same cells
        assert_eq!(client.notebook_id(), Some("sync-test".to_string()));
        let cells = client.get_cells();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].source, "import numpy");
        assert_eq!(cells[0].outputs.len(), 1);
    }

    #[test]
    fn test_concurrent_cell_adds_merge() {
        let mut server = NotebookDoc::new("merge-test");
        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };

        let mut server_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Initial sync to share the base document
        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        // Both add different cells concurrently (before syncing)
        server.add_cell(0, "server-cell", "code").unwrap();
        server.update_source("server-cell", "# server").unwrap();

        client.add_cell(0, "client-cell", "markdown").unwrap();
        client.update_source("client-cell", "# client").unwrap();

        // Sync again
        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        // Both should have both cells (order may vary due to CRDT resolution)
        let server_cells = server.get_cells();
        let client_cells = client.get_cells();
        assert_eq!(server_cells.len(), 2);
        assert_eq!(client_cells.len(), 2);

        let server_ids: Vec<&str> = server_cells.iter().map(|c| c.id.as_str()).collect();
        let client_ids: Vec<&str> = client_cells.iter().map(|c| c.id.as_str()).collect();
        assert!(server_ids.contains(&"server-cell"));
        assert!(server_ids.contains(&"client-cell"));
        assert_eq!(server_ids, client_ids); // Same order after merge
    }

    #[test]
    fn test_get_cells_from_doc_helper() {
        let mut doc = NotebookDoc::new("helper-test");
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "x = 1").unwrap();

        let cells = get_cells_from_doc(&doc.doc);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "c1");
        assert_eq!(cells[0].source, "x = 1");
    }

    #[test]
    fn test_get_cells_from_empty_doc() {
        let doc = AutoCommit::new();
        let cells = get_cells_from_doc(&doc);
        assert!(cells.is_empty());
    }
}
