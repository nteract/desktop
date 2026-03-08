//! Automerge-backed notebook document for cross-window sync.
//!
//! Also re-exports typed notebook metadata structs (`metadata` module) so all
//! peers (daemon, Tauri relay, WASM frontend, Python bindings) share one
//! definition of kernelspec, dependencies, and trust metadata.
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

pub mod metadata;

use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, AutomergeError, ObjId, ObjType, ReadDoc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "persistence")]
use log::{info, warn};
#[cfg(feature = "persistence")]
use std::path::Path;

/// Tracks the last-written state for a stream output in a cell.
/// Used by `upsert_stream_output` for in-place update validation.
#[derive(Debug, Clone)]
pub struct StreamOutputState {
    /// Index in the cell's outputs list
    pub index: usize,
    /// Manifest hash we last wrote at this index
    pub manifest_hash: String,
}

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
    /// Access the underlying Automerge document (read-only).
    pub fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// Access the underlying Automerge document (mutable).
    pub fn doc_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }
}

// ── Typed metadata helpers ──────────────────────────────────────────

impl NotebookDoc {
    /// Read the notebook metadata as a typed snapshot.
    pub fn get_metadata_snapshot(&self) -> Option<metadata::NotebookMetadataSnapshot> {
        let json = self.get_metadata(metadata::NOTEBOOK_METADATA_KEY)?;
        serde_json::from_str(&json).ok()
    }

    /// Write a typed metadata snapshot to the document.
    pub fn set_metadata_snapshot(
        &mut self,
        snapshot: &metadata::NotebookMetadataSnapshot,
    ) -> Result<(), AutomergeError> {
        let json = serde_json::to_string(snapshot)
            .map_err(|e| AutomergeError::InvalidObjId(format!("serialize metadata: {}", e)))?;
        self.set_metadata(metadata::NOTEBOOK_METADATA_KEY, &json)
    }

    /// Detect the notebook runtime from metadata (kernelspec + language_info).
    ///
    /// Returns `"python"`, `"deno"`, or `None` for unknown runtimes.
    pub fn detect_runtime(&self) -> Option<String> {
        let snapshot = self.get_metadata_snapshot()?;

        // Check kernelspec.name first (most reliable)
        if let Some(ref ks) = snapshot.kernelspec {
            let name = ks.name.to_lowercase();
            if name.contains("deno") {
                return Some("deno".to_string());
            }
            if name.contains("python") {
                return Some("python".to_string());
            }
            // Check kernelspec.language
            if let Some(ref lang) = ks.language {
                let lang_lower = lang.to_lowercase();
                if lang_lower == "typescript" || lang_lower == "javascript" {
                    return Some("deno".to_string());
                }
                if lang_lower == "python" {
                    return Some("python".to_string());
                }
            }
        }

        // Fall back to language_info.name
        if let Some(ref li) = snapshot.language_info {
            let name = li.name.to_lowercase();
            if name == "typescript" || name == "javascript" {
                return Some("deno".to_string());
            }
            if name == "python" {
                return Some("python".to_string());
            }
        }

        None
    }
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

    /// Load from file or create a new document if the file doesn't exist.
    ///
    /// If the file exists but is corrupt (read or decode failure), the broken
    /// file is renamed to `{path}.corrupt` and a fresh document is created.
    /// This avoids silent data loss while still allowing the daemon to proceed.
    #[cfg(feature = "persistence")]
    pub fn load_or_create(path: &Path, notebook_id: &str) -> Self {
        if path.exists() {
            match std::fs::read(path) {
                Ok(data) => match AutoCommit::load(&data) {
                    Ok(doc) => {
                        info!("[notebook-doc] Loaded from {:?} for {}", path, notebook_id);
                        return Self { doc };
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-doc] Corrupt doc at {:?} for {}: {}. \
                             Preserving as .corrupt and creating fresh doc.",
                            path, notebook_id, e
                        );
                        Self::preserve_corrupt(path);
                    }
                },
                Err(e) => {
                    warn!(
                        "[notebook-doc] Failed to read {:?} for {}: {}. \
                         Preserving as .corrupt and creating fresh doc.",
                        path, notebook_id, e
                    );
                    Self::preserve_corrupt(path);
                }
            }
        }

        info!(
            "[notebook-doc] Creating new doc for {} (path: {:?})",
            notebook_id, path
        );
        Self::new(notebook_id)
    }

    /// Rename a corrupt persisted file to `{path}.corrupt` for diagnostics.
    #[cfg(feature = "persistence")]
    fn preserve_corrupt(path: &Path) {
        let corrupt_path = path.with_extension("automerge.corrupt");
        if let Err(e) = std::fs::rename(path, &corrupt_path) {
            warn!(
                "[notebook-doc] Failed to rename corrupt file {:?} → {:?}: {}",
                path, corrupt_path, e
            );
        } else {
            warn!(
                "[notebook-doc] Corrupt file preserved at {:?}",
                corrupt_path
            );
        }
    }

    /// Serialize the document to bytes.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Save the document to a file.
    #[cfg(feature = "persistence")]
    pub fn save_to_file(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = self.save();
        std::fs::write(path, data)
    }

    // ── Notebook ID ─────────────────────────────────────────────────

    /// Read the notebook ID from the document.
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

    /// Update or insert a stream output for a cell.
    ///
    /// If `known_state` is provided, validates that the output at the cached index
    /// still has the expected manifest hash. If validation passes, updates in place.
    /// If validation fails (hash mismatch, index out of bounds, or no state), appends
    /// a new output.
    ///
    /// This validation protects against:
    /// - External clear operations (another peer, frontend-initiated)
    /// - Individual output deletion
    /// - CRDT modifications between stream messages
    ///
    /// Returns (updated: bool, output_index: usize) where updated is true if an
    /// existing output was updated, false if a new output was appended.
    pub fn upsert_stream_output(
        &mut self,
        cell_id: &str,
        _stream_name: &str,
        output_ref: &str,
        known_state: Option<&StreamOutputState>,
    ) -> Result<(bool, usize), AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok((false, 0)),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Ok((false, 0)),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Ok((false, 0)),
        };
        let outputs_id = match self.list_id(&cell_obj, "outputs") {
            Some(id) => id,
            None => return Ok((false, 0)),
        };

        let output_count = self.doc.length(&outputs_id);

        // Validate cached state if provided
        // Only update in-place if:
        // 1. Index is valid and points to the last output (nothing appended after it)
        // 2. Hash matches what we last wrote
        // This ensures interleaved stdout/stderr don't corrupt ordering.
        if let Some(state) = known_state {
            // Must be the last output - if something was appended after (e.g., stderr
            // between two stdout messages), we should append instead of updating
            if state.index + 1 == output_count {
                // Read what's currently at that index
                if let Ok(Some((value, _))) = self.doc.get(&outputs_id, state.index) {
                    if let Ok(current_hash) = value.into_string() {
                        if current_hash == state.manifest_hash {
                            // ✓ Validated! Safe to update in place
                            self.doc.put(&outputs_id, state.index, output_ref)?;
                            return Ok((true, state.index));
                        }
                    }
                }
            }
            // Validation failed - fall through to append
        }

        // No valid state, append new output
        self.doc.insert(&outputs_id, output_count, output_ref)?;
        Ok((false, output_count))
    }

    /// Update an output by display_id across all cells.
    ///
    /// This is used for `update_display_data` messages which mutate an existing
    /// output in place (e.g., progress bars). The display_id may appear in any
    /// cell's outputs.
    ///
    /// Returns true if an output was found and updated.
    pub fn update_output_by_display_id(
        &mut self,
        display_id: &str,
        new_data: &serde_json::Value,
        new_metadata: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Ok(false),
        };

        let cell_count = self.doc.length(&cells_id);
        for cell_idx in 0..cell_count {
            let cell_obj = match self.cell_at_index(&cells_id, cell_idx) {
                Some(o) => o,
                None => continue,
            };
            let outputs_id = match self.list_id(&cell_obj, "outputs") {
                Some(id) => id,
                None => continue,
            };

            let output_count = self.doc.length(&outputs_id);
            for output_idx in 0..output_count {
                // Get output string and parse as JSON
                let output_str: Option<String> = self
                    .doc
                    .get(&outputs_id, output_idx)
                    .ok()
                    .flatten()
                    .and_then(|(v, _)| v.into_string().ok());

                let output_str = match output_str {
                    Some(s) => s,
                    None => continue,
                };

                // Parse and check display_id
                let mut output_json: serde_json::Value = match serde_json::from_str(&output_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let matches = output_json
                    .get("transient")
                    .and_then(|t| t.get("display_id"))
                    .and_then(|d| d.as_str())
                    == Some(display_id);

                if matches {
                    // Update data and metadata in place
                    output_json["data"] = new_data.clone();
                    output_json["metadata"] = serde_json::Value::Object(new_metadata.clone());

                    // Write back
                    let updated_str = output_json.to_string();
                    self.doc.put(&outputs_id, output_idx, updated_str)?;
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Clear all outputs from a cell.
    pub fn clear_outputs(&mut self, cell_id: &str) -> Result<bool, AutomergeError> {
        self.set_outputs(cell_id, &[])
    }

    /// Get all outputs from all cells.
    ///
    /// Returns a list of (cell_id, output_index, output_string).
    /// Used by manifest-aware UpdateDisplayData handling.
    pub fn get_all_outputs(&self) -> Vec<(String, usize, String)> {
        let mut results = Vec::new();
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return results,
        };

        let cell_count = self.doc.length(&cells_id);
        for cell_idx in 0..cell_count {
            let cell_obj = match self.cell_at_index(&cells_id, cell_idx) {
                Some(o) => o,
                None => continue,
            };

            // Get cell_id
            let cell_id: Option<String> = self
                .doc
                .get(&cell_obj, "id")
                .ok()
                .flatten()
                .and_then(|(v, _)| v.into_string().ok());
            let cell_id = match cell_id {
                Some(id) => id,
                None => continue,
            };

            let outputs_id = match self.list_id(&cell_obj, "outputs") {
                Some(id) => id,
                None => continue,
            };

            let output_count = self.doc.length(&outputs_id);
            for output_idx in 0..output_count {
                let output_str: Option<String> = self
                    .doc
                    .get(&outputs_id, output_idx)
                    .ok()
                    .flatten()
                    .and_then(|(v, _)| v.into_string().ok());

                if let Some(s) = output_str {
                    results.push((cell_id.clone(), output_idx, s));
                }
            }
        }

        results
    }

    /// Replace an output by cell_id and index.
    ///
    /// Used by manifest-aware UpdateDisplayData handling.
    pub fn replace_output(
        &mut self,
        cell_id: &str,
        output_idx: usize,
        new_output: &str,
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

        let outputs_id = match self.list_id(&cell_obj, "outputs") {
            Some(id) => id,
            None => return Ok(false),
        };

        // Check that output_idx is valid
        if output_idx >= self.doc.length(&outputs_id) {
            return Ok(false);
        }

        self.doc.put(&outputs_id, output_idx, new_output)?;
        Ok(true)
    }

    // ── Execution count ─────────────────────────────────────────────

    /// Set the execution count for a cell. Pass "null" or a number string like "5".
    pub fn set_execution_count(
        &mut self,
        cell_id: &str,
        count: &str,
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

        self.doc.put(&cell_obj, "execution_count", count)?;
        Ok(true)
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

/// Read the typed notebook metadata snapshot from a raw `AutoCommit` document.
///
/// This is the free-function counterpart of `NotebookDoc::get_metadata_snapshot`,
/// for use by the sync client which holds a raw `AutoCommit` instead of a
/// `NotebookDoc`.
pub fn get_metadata_snapshot_from_doc(
    doc: &AutoCommit,
) -> Option<metadata::NotebookMetadataSnapshot> {
    let json = get_metadata_from_doc(doc, metadata::NOTEBOOK_METADATA_KEY)?;
    serde_json::from_str(&json).ok()
}

/// Set a metadata value in a raw `AutoCommit` document.
///
/// Creates the metadata map if it doesn't exist. This is the free-function
/// counterpart of `NotebookDoc::set_metadata`.
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

/// Compute a safe filename for persisting a notebook document.
///
/// Hashes the notebook_id (which could be a file path with special characters)
/// using SHA-256 to produce a safe, deterministic filename.
#[cfg(feature = "persistence")]
pub fn notebook_doc_filename(notebook_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = hex::encode(Sha256::digest(notebook_id.as_bytes()));
    format!("{}.automerge", hash)
}

/// Read cells from a raw AutoCommit document (used by the sync client).
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
    fn test_set_execution_count() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();

        doc.set_execution_count("cell-1", "42").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.execution_count, "42");

        doc.set_execution_count("cell-1", "null").unwrap();
        let cell = doc.get_cell("cell-1").unwrap();
        assert_eq!(cell.execution_count, "null");
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
        doc.set_execution_count("cell-1", "1").unwrap();
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
        assert_eq!(cells[0].execution_count, "1");
        assert_eq!(cells[0].outputs.len(), 1);
        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].source, "# Hello");
    }

    #[test]
    #[cfg(feature = "persistence")]
    fn test_save_to_file_and_load_or_create() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("notebook.automerge");

        let mut doc = NotebookDoc::new("file-test");
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "print(1)").unwrap();
        doc.save_to_file(&path).unwrap();

        let loaded = NotebookDoc::load_or_create(&path, "file-test");
        assert_eq!(loaded.notebook_id(), Some("file-test".to_string()));
        let cells = loaded.get_cells();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].source, "print(1)");
    }

    #[test]
    #[cfg(feature = "persistence")]
    fn test_load_or_create_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.automerge");

        let doc = NotebookDoc::load_or_create(&path, "new-nb");
        assert_eq!(doc.notebook_id(), Some("new-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);
    }

    #[test]
    #[cfg(feature = "persistence")]
    fn test_load_or_create_corrupt_file_preserved() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("corrupt.automerge");

        // Write garbage data
        std::fs::write(&path, b"this is not a valid automerge document").unwrap();
        assert!(path.exists());

        // load_or_create should create a fresh doc
        let doc = NotebookDoc::load_or_create(&path, "corrupt-nb");
        assert_eq!(doc.notebook_id(), Some("corrupt-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);

        // Original file should have been renamed to .corrupt
        let corrupt_path = path.with_extension("automerge.corrupt");
        assert!(corrupt_path.exists(), "corrupt file should be preserved");
        assert_eq!(
            std::fs::read(&corrupt_path).unwrap(),
            b"this is not a valid automerge document"
        );
    }

    #[test]
    fn test_sync_between_two_docs() {
        // Server creates a notebook with cells
        let mut server = NotebookDoc::new("sync-test");
        server.add_cell(0, "cell-1", "code").unwrap();
        server.update_source("cell-1", "import numpy").unwrap();
        server.set_execution_count("cell-1", "1").unwrap();
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
        assert_eq!(cells[0].execution_count, "1");
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
    #[cfg(feature = "persistence")]
    fn test_notebook_doc_filename_deterministic() {
        let f1 = notebook_doc_filename("/path/to/notebook.ipynb");
        let f2 = notebook_doc_filename("/path/to/notebook.ipynb");
        assert_eq!(f1, f2);
        assert!(f1.ends_with(".automerge"));
        // Different paths produce different filenames
        let f3 = notebook_doc_filename("/other/path.ipynb");
        assert_ne!(f1, f3);
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
