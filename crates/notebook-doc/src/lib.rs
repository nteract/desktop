//! Automerge-backed notebook document for cross-window sync.
//!
//! Also re-exports typed notebook metadata structs (`metadata` module) so all
//! peers (daemon, WASM frontend, Python bindings) share one
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
//!       metadata: Str             ← JSON-encoded cell metadata object
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CellSnapshot {
    pub id: String,
    /// "code", "markdown", or "raw"
    pub cell_type: String,
    pub source: String,
    /// JSON-encoded execution count: a number string like "5" or "null"
    pub execution_count: String,
    /// JSON-encoded Jupyter output objects (will become manifest hashes in Phase 5)
    pub outputs: Vec<String>,
    /// Cell metadata (arbitrary JSON object, preserves unknown keys)
    #[serde(default = "default_empty_object")]
    pub metadata: serde_json::Value,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

impl CellSnapshot {
    /// Returns true if the cell source should be hidden (JupyterLab convention).
    pub fn is_source_hidden(&self) -> bool {
        self.metadata
            .get("jupyter")
            .and_then(|j| j.get("source_hidden"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Returns true if the cell outputs should be hidden (JupyterLab convention).
    pub fn is_outputs_hidden(&self) -> bool {
        self.metadata
            .get("jupyter")
            .and_then(|j| j.get("outputs_hidden"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Returns true if the cell output area is collapsed (Classic Notebook convention).
    pub fn is_collapsed(&self) -> bool {
        self.metadata
            .get("collapsed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Returns cell tags (empty vec if none).
    pub fn tags(&self) -> Vec<String> {
        self.metadata
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
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

    /// Wrap an existing AutoCommit document.
    ///
    /// Use this when you need to call NotebookDoc methods on an AutoCommit
    /// that was constructed elsewhere (e.g., in a sync client).
    pub fn wrap(doc: AutoCommit) -> Self {
        Self { doc }
    }

    /// Consume the NotebookDoc and return the underlying AutoCommit.
    ///
    /// Use this after wrapping to get back the modified AutoCommit.
    pub fn into_inner(self) -> AutoCommit {
        self.doc
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
    /// Delegates to [`metadata::NotebookMetadataSnapshot::detect_runtime`].
    pub fn detect_runtime(&self) -> Option<String> {
        self.get_metadata_snapshot()?.detect_runtime()
    }

    // ── UV dependency convenience methods ─────────────────────────

    /// Add a UV dependency, deduplicating by package name (case-insensitive).
    pub fn add_uv_dependency(&mut self, pkg: &str) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.add_uv_dependency(pkg);
        self.set_metadata_snapshot(&snapshot)
    }

    /// Remove a UV dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_uv_dependency(&mut self, pkg: &str) -> Result<bool, AutomergeError> {
        let Some(mut snapshot) = self.get_metadata_snapshot() else {
            return Ok(false);
        };
        let removed = snapshot.remove_uv_dependency(pkg);
        if removed {
            self.set_metadata_snapshot(&snapshot)?;
        }
        Ok(removed)
    }

    /// Clear the UV section entirely (deps + requires-python).
    pub fn clear_uv_section(&mut self) -> Result<(), AutomergeError> {
        if let Some(mut snapshot) = self.get_metadata_snapshot() {
            snapshot.clear_uv_section();
            self.set_metadata_snapshot(&snapshot)
        } else {
            Ok(())
        }
    }

    /// Set UV requires-python constraint, preserving deps.
    /// Creates the metadata snapshot and UV section if absent.
    pub fn set_uv_requires_python(
        &mut self,
        requires_python: Option<String>,
    ) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_uv_requires_python(requires_python);
        self.set_metadata_snapshot(&snapshot)
    }

    // ── Conda dependency convenience methods ──────────────────────

    /// Add a Conda dependency, deduplicating by package name (case-insensitive).
    pub fn add_conda_dependency(&mut self, pkg: &str) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.add_conda_dependency(pkg);
        self.set_metadata_snapshot(&snapshot)
    }

    /// Remove a Conda dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_conda_dependency(&mut self, pkg: &str) -> Result<bool, AutomergeError> {
        let Some(mut snapshot) = self.get_metadata_snapshot() else {
            return Ok(false);
        };
        let removed = snapshot.remove_conda_dependency(pkg);
        if removed {
            self.set_metadata_snapshot(&snapshot)?;
        }
        Ok(removed)
    }

    /// Clear the Conda section entirely.
    pub fn clear_conda_section(&mut self) -> Result<(), AutomergeError> {
        if let Some(mut snapshot) = self.get_metadata_snapshot() {
            snapshot.clear_conda_section();
            self.set_metadata_snapshot(&snapshot)
        } else {
            Ok(())
        }
    }

    /// Set Conda channels, preserving deps and python.
    pub fn set_conda_channels(&mut self, channels: Vec<String>) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_conda_channels(channels);
        self.set_metadata_snapshot(&snapshot)
    }

    /// Set Conda python version, preserving deps and channels.
    pub fn set_conda_python(&mut self, python: Option<String>) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_conda_python(python);
        self.set_metadata_snapshot(&snapshot)
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

    /// Create a document with zero operations for sync-only bootstrap.
    ///
    /// Unlike `new()`, this does not create a cells list, metadata map, or
    /// notebook_id. The sync protocol populates everything from the peer.
    /// All read methods (`cell_count`, `get_cells`, etc.) handle the missing
    /// keys gracefully (return 0 / empty).
    pub fn empty() -> Self {
        Self {
            doc: AutoCommit::new(),
        }
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
    /// Returns `Ok(())` on success. The cell starts with empty source, no outputs, and empty metadata.
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
        self.doc.put(&cell_map, "metadata", "{}")?;
        Ok(())
    }

    /// Insert a fully-populated cell at the given index in a single operation.
    ///
    /// Unlike calling `add_cell` + `update_source` + `set_outputs` +
    /// `set_execution_count` sequentially, this method reuses the `ObjId`
    /// returned by each Automerge insertion — no `find_cell_index` lookup
    /// is needed. This eliminates the O(n) linear scan per operation that
    /// makes sequential calls O(n²) during bulk loads.
    #[allow(clippy::too_many_arguments)]
    pub fn add_cell_full(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
        source: &str,
        outputs: &[String],
        execution_count: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), AutomergeError> {
        let cells_id = self
            .cells_list_id()
            .ok_or_else(|| AutomergeError::InvalidObjId("cells list not found".into()))?;

        let len = self.doc.length(&cells_id);
        let index = index.min(len);

        let cell_map = self.doc.insert_object(&cells_id, index, ObjType::Map)?;
        self.doc.put(&cell_map, "id", cell_id)?;
        self.doc.put(&cell_map, "cell_type", cell_type)?;

        let source_id = self.doc.put_object(&cell_map, "source", ObjType::Text)?;
        if !source.is_empty() {
            // splice_text directly inserts into the empty Text CRDT.
            // update_text would run a Myers diff from "" → source, which is
            // O(n) per character and gets progressively slower as the
            // Automerge document grows.
            self.doc.splice_text(&source_id, 0, 0, source)?;
        }

        self.doc
            .put(&cell_map, "execution_count", execution_count)?;

        let outputs_id = self.doc.put_object(&cell_map, "outputs", ObjType::List)?;
        for (i, output) in outputs.iter().enumerate() {
            self.doc.insert(&outputs_id, i, output.as_str())?;
        }

        // Store metadata as JSON string
        let metadata_str = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string());
        self.doc.put(&cell_map, "metadata", metadata_str)?;

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

    /// Remove all cells from the document.
    ///
    /// Used to clean up after a failed streaming load so the next
    /// connection can retry from a clean state.
    pub fn clear_all_cells(&mut self) -> Result<(), AutomergeError> {
        if let Some(cells_id) = self.cells_list_id() {
            let len = self.doc.length(&cells_id);
            // Delete from the end to avoid index shifting
            for i in (0..len).rev() {
                self.doc.delete(&cells_id, i)?;
            }
        }
        Ok(())
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

    // ── Cell metadata ──────────────────────────────────────────────

    /// Get the raw metadata Value for a cell.
    ///
    /// Returns `None` if the cell doesn't exist.
    /// Returns `Some({})` if the cell exists but has no or invalid metadata.
    pub fn get_cell_metadata(&self, cell_id: &str) -> Option<serde_json::Value> {
        let cells_id = self.cells_list_id()?;
        let idx = self.find_cell_index(&cells_id, cell_id)?;
        let cell_obj = self.cell_at_index(&cells_id, idx)?;

        // Cell exists - return its metadata or empty object if missing/invalid
        Some(
            read_str(&self.doc, &cell_obj, "metadata")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({})),
        )
    }

    /// Set the entire metadata object for a cell.
    ///
    /// Note: Metadata is stored as a JSON-encoded string, not as a CRDT structure.
    /// Concurrent edits from multiple peers will result in last-write-wins semantics.
    /// Use `update_cell_metadata_at` for path-based updates when possible.
    pub fn set_cell_metadata(
        &mut self,
        cell_id: &str,
        metadata: &serde_json::Value,
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

        let metadata_str = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string());
        self.doc.put(&cell_obj, "metadata", metadata_str)?;
        Ok(true)
    }

    /// Update a nested path within cell metadata.
    ///
    /// Creates intermediate objects as needed. For example:
    /// `update_cell_metadata_at("cell-1", &["jupyter", "source_hidden"], json!(true))`
    /// will create `{"jupyter": {"source_hidden": true}}` if metadata was `{}`.
    ///
    /// Note: This performs a read-modify-write on the JSON string. Concurrent updates
    /// to different paths may conflict (last-write-wins), but this is rare in practice
    /// since metadata updates are typically user-initiated actions.
    pub fn update_cell_metadata_at(
        &mut self,
        cell_id: &str,
        path: &[&str],
        value: serde_json::Value,
    ) -> Result<bool, AutomergeError> {
        if path.is_empty() {
            return self.set_cell_metadata(cell_id, &value);
        }

        let mut metadata = self
            .get_cell_metadata(cell_id)
            .unwrap_or_else(|| serde_json::json!({}));

        // Navigate to the parent of the target key, creating objects as needed
        let mut current = &mut metadata;
        for key in &path[..path.len() - 1] {
            if !current.is_object() {
                *current = serde_json::json!({});
            }
            let obj = current.as_object_mut().unwrap();
            if !obj.contains_key(*key) {
                obj.insert((*key).to_string(), serde_json::json!({}));
            }
            current = obj.get_mut(*key).unwrap();
        }

        // Set the final key
        if !current.is_object() {
            *current = serde_json::json!({});
        }
        let final_key = path[path.len() - 1];
        current
            .as_object_mut()
            .unwrap()
            .insert(final_key.to_string(), value);

        self.set_cell_metadata(cell_id, &metadata)
    }

    /// Set whether the cell source should be hidden (JupyterLab convention).
    pub fn set_cell_source_hidden(
        &mut self,
        cell_id: &str,
        hidden: bool,
    ) -> Result<bool, AutomergeError> {
        self.update_cell_metadata_at(
            cell_id,
            &["jupyter", "source_hidden"],
            serde_json::json!(hidden),
        )
    }

    /// Set whether the cell outputs should be hidden (JupyterLab convention).
    pub fn set_cell_outputs_hidden(
        &mut self,
        cell_id: &str,
        hidden: bool,
    ) -> Result<bool, AutomergeError> {
        self.update_cell_metadata_at(
            cell_id,
            &["jupyter", "outputs_hidden"],
            serde_json::json!(hidden),
        )
    }

    /// Set the cell tags.
    pub fn set_cell_tags(
        &mut self,
        cell_id: &str,
        tags: Vec<String>,
    ) -> Result<bool, AutomergeError> {
        self.update_cell_metadata_at(cell_id, &["tags"], serde_json::json!(tags))
    }

    // ── Notebook metadata ──────────────────────────────────────────

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

        // Read metadata (JSON string -> Value)
        let metadata = read_str(&self.doc, cell_obj, "metadata")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        Some(CellSnapshot {
            id,
            cell_type,
            source,
            execution_count,
            outputs,
            metadata,
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

            // Read metadata (JSON string -> Value)
            let metadata = read_str(doc, &cell_obj, "metadata")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));

            Some(CellSnapshot {
                id,
                cell_type,
                source,
                execution_count,
                outputs,
                metadata,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_doc_has_no_cells_or_metadata() {
        let doc = NotebookDoc::empty();
        assert_eq!(doc.notebook_id(), None);
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(doc.get_cells(), vec![]);
        assert_eq!(doc.get_metadata("runtime"), None);
        assert!(doc.get_metadata_snapshot().is_none());
        assert!(doc.detect_runtime().is_none());
    }

    #[test]
    fn test_empty_doc_set_metadata() {
        let mut doc = NotebookDoc::empty();
        // set_metadata should work even without a pre-existing metadata map
        let result = doc.set_metadata("runtime", "python");
        assert!(result.is_ok());
        assert_eq!(doc.get_metadata("runtime"), Some("python".to_string()));
    }

    #[test]
    fn test_empty_doc_sync_with_populated_doc() {
        use automerge::sync;

        let mut daemon = NotebookDoc::new("test-notebook");
        daemon.add_cell(0, "cell-1", "code").unwrap();
        daemon.update_source("cell-1", "print('hello')").unwrap();

        let mut empty = NotebookDoc::empty();
        let mut daemon_state = sync::State::new();
        let mut empty_state = sync::State::new();

        // Sync until convergence
        for _ in 0..10 {
            let msg_from_daemon = daemon.generate_sync_message(&mut daemon_state);
            let msg_from_empty = empty.generate_sync_message(&mut empty_state);
            if msg_from_daemon.is_none() && msg_from_empty.is_none() {
                break;
            }
            if let Some(m) = msg_from_daemon {
                empty.receive_sync_message(&mut empty_state, m).unwrap();
            }
            if let Some(m) = msg_from_empty {
                daemon.receive_sync_message(&mut daemon_state, m).unwrap();
            }
        }

        assert_eq!(empty.cell_count(), 1);
        let cell = empty.get_cell("cell-1").unwrap();
        assert_eq!(cell.source, "print('hello')");
        assert_eq!(empty.notebook_id(), Some("test-notebook".to_string()));
    }

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

    // ── Sync integration tests (WASM sync protocol coverage) ──────────

    /// Helper to sync two docs to convergence.
    fn sync_docs(
        doc_a: &mut NotebookDoc,
        state_a: &mut sync::State,
        doc_b: &mut NotebookDoc,
        state_b: &mut sync::State,
        max_rounds: usize,
    ) {
        for _ in 0..max_rounds {
            let msg_a = doc_a.generate_sync_message(state_a);
            let msg_b = doc_b.generate_sync_message(state_b);
            if msg_a.is_none() && msg_b.is_none() {
                break;
            }
            if let Some(msg) = msg_a {
                doc_b.receive_sync_message(state_b, msg).unwrap();
            }
            if let Some(msg) = msg_b {
                doc_a.receive_sync_message(state_a, msg).unwrap();
            }
        }
    }

    /// Tests output sync from daemon to client AFTER initial sync.
    /// This is the exact flow that broke in #617.
    #[test]
    fn test_output_sync_from_daemon_to_client() {
        // Daemon creates notebook with a cell
        let mut daemon = NotebookDoc::new("output-sync-test");
        daemon.add_cell(0, "cell-1", "code").unwrap();
        daemon.update_source("cell-1", "print('hello')").unwrap();

        // Client starts empty and syncs
        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        // Verify initial sync worked
        assert_eq!(client.cell_count(), 1);
        let cell = client.get_cell("cell-1").unwrap();
        assert!(cell.outputs.is_empty());

        // Daemon appends output (simulating kernel execution)
        daemon
            .append_output(
                "cell-1",
                r#"{"output_type":"stream","name":"stdout","text":"hello\n"}"#,
            )
            .unwrap();
        daemon.set_execution_count("cell-1", "1").unwrap();

        // Sync again - this is where #617 failed
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        // Client should see the output
        let cell = client.get_cell("cell-1").unwrap();
        assert_eq!(cell.outputs.len(), 1);
        assert!(cell.outputs[0].contains("stdout"));
        assert_eq!(cell.execution_count, "1");
    }

    /// Tests execution count sync propagates correctly.
    #[test]
    fn test_execution_count_sync() {
        let mut daemon = NotebookDoc::new("exec-count-test");
        daemon.add_cell(0, "cell-1", "code").unwrap();

        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        // Daemon sets execution count
        daemon.set_execution_count("cell-1", "42").unwrap();
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        assert_eq!(client.get_cell("cell-1").unwrap().execution_count, "42");

        // Daemon updates execution count again
        daemon.set_execution_count("cell-1", "43").unwrap();
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        assert_eq!(client.get_cell("cell-1").unwrap().execution_count, "43");
    }

    /// Tests clear_outputs syncs correctly.
    #[test]
    fn test_clear_outputs_sync() {
        let mut daemon = NotebookDoc::new("clear-outputs-test");
        daemon.add_cell(0, "cell-1", "code").unwrap();
        daemon.append_output("cell-1", "output1").unwrap();
        daemon.append_output("cell-1", "output2").unwrap();

        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );
        assert_eq!(client.get_cell("cell-1").unwrap().outputs.len(), 2);

        // Daemon clears outputs
        daemon.clear_outputs("cell-1").unwrap();
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        assert!(client.get_cell("cell-1").unwrap().outputs.is_empty());
    }

    /// Tests bidirectional sync: client adds cell, daemon writes output.
    #[test]
    fn test_bidirectional_sync_client_adds_daemon_outputs() {
        let mut daemon = NotebookDoc::new("bidirectional-test");
        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Initial sync
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        // Client adds a cell with source code
        client.add_cell(0, "user-cell", "code").unwrap();
        client.update_source("user-cell", "x = 1 + 1").unwrap();

        // Sync - daemon should see the cell
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );
        assert_eq!(daemon.cell_count(), 1);
        assert_eq!(daemon.get_cell("user-cell").unwrap().source, "x = 1 + 1");

        // Daemon executes and writes output
        daemon
            .append_output(
                "user-cell",
                r#"{"output_type":"execute_result","data":{"text/plain":"2"}}"#,
            )
            .unwrap();
        daemon.set_execution_count("user-cell", "1").unwrap();

        // Sync back - client should see the output
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        let cell = client.get_cell("user-cell").unwrap();
        assert_eq!(cell.source, "x = 1 + 1"); // Still has its source
        assert_eq!(cell.outputs.len(), 1);
        assert!(cell.outputs[0].contains("execute_result"));
        assert_eq!(cell.execution_count, "1");
    }

    /// Tests three-peer sync: daemon + two clients.
    #[test]
    fn test_three_peer_sync() {
        let mut daemon = NotebookDoc::new("three-peer-test");
        let mut client1 = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut client2 = NotebookDoc {
            doc: AutoCommit::new(),
        };
        let mut daemon_state1 = sync::State::new();
        let mut daemon_state2 = sync::State::new();
        let mut client1_state = sync::State::new();
        let mut client2_state = sync::State::new();

        // Initial sync all three
        sync_docs(
            &mut daemon,
            &mut daemon_state1,
            &mut client1,
            &mut client1_state,
            10,
        );
        sync_docs(
            &mut daemon,
            &mut daemon_state2,
            &mut client2,
            &mut client2_state,
            10,
        );

        // Daemon adds a cell with output
        daemon.add_cell(0, "daemon-cell", "code").unwrap();
        daemon.update_source("daemon-cell", "print(42)").unwrap();
        daemon
            .append_output("daemon-cell", r#"{"text":"42"}"#)
            .unwrap();

        // Sync both clients
        sync_docs(
            &mut daemon,
            &mut daemon_state1,
            &mut client1,
            &mut client1_state,
            10,
        );
        sync_docs(
            &mut daemon,
            &mut daemon_state2,
            &mut client2,
            &mut client2_state,
            10,
        );

        // Both clients should have identical state
        let cells1 = client1.get_cells();
        let cells2 = client2.get_cells();
        assert_eq!(cells1.len(), 1);
        assert_eq!(cells2.len(), 1);
        assert_eq!(cells1[0].id, cells2[0].id);
        assert_eq!(cells1[0].source, cells2[0].source);
        assert_eq!(cells1[0].outputs, cells2[0].outputs);
    }

    /// Tests empty-to-full bootstrap: fresh client receives daemon's first sync.
    /// This tests the pipe-mode path from #619/#622.
    #[test]
    fn test_empty_to_full_bootstrap() {
        // Daemon has existing content
        let mut daemon = NotebookDoc::new("bootstrap-test");
        daemon.add_cell(0, "cell-1", "code").unwrap();
        daemon
            .update_source("cell-1", "import numpy as np")
            .unwrap();
        daemon.set_execution_count("cell-1", "1").unwrap();
        daemon
            .append_output("cell-1", r#"{"output_type":"stream"}"#)
            .unwrap();
        daemon.add_cell(1, "cell-2", "markdown").unwrap();
        daemon.update_source("cell-2", "# Analysis").unwrap();
        daemon.set_metadata("custom_key", "custom_value").unwrap();

        // Client starts completely empty (zero operations)
        let mut client = NotebookDoc {
            doc: AutoCommit::new(),
        };
        assert_eq!(client.cell_count(), 0);
        assert!(client.notebook_id().is_none());

        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Single sync pass should transfer everything
        sync_docs(
            &mut daemon,
            &mut daemon_state,
            &mut client,
            &mut client_state,
            10,
        );

        // Client should have all content
        assert_eq!(client.notebook_id(), Some("bootstrap-test".to_string()));
        assert_eq!(client.cell_count(), 2);

        let cells = client.get_cells();
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].source, "import numpy as np");
        assert_eq!(cells[0].execution_count, "1");
        assert_eq!(cells[0].outputs.len(), 1);

        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].source, "# Analysis");

        assert_eq!(
            client.get_metadata("custom_key"),
            Some("custom_value".to_string())
        );
    }

    #[test]
    fn test_add_cell_full_populates_all_fields() {
        let mut doc = NotebookDoc::new("nb-full");
        doc.add_cell_full(
            0,
            "cell-full",
            "code",
            "print('hello')",
            &["hash1".to_string(), "hash2".to_string()],
            "42",
            &serde_json::json!({"tags": ["test"]}),
        )
        .unwrap();

        assert_eq!(doc.cell_count(), 1);
        let cell = doc.get_cell("cell-full").unwrap();
        assert_eq!(cell.id, "cell-full");
        assert_eq!(cell.cell_type, "code");
        assert_eq!(cell.source, "print('hello')");
        assert_eq!(cell.execution_count, "42");
        assert_eq!(cell.outputs, vec!["hash1", "hash2"]);
        assert_eq!(cell.tags(), vec!["test"]);
    }

    #[test]
    fn test_add_cell_full_empty_source() {
        let mut doc = NotebookDoc::new("nb-empty-src");
        doc.add_cell_full(
            0,
            "cell-es",
            "code",
            "",
            &[],
            "null",
            &serde_json::json!({}),
        )
        .unwrap();

        let cell = doc.get_cell("cell-es").unwrap();
        assert_eq!(cell.source, "");
        assert_eq!(cell.execution_count, "null");
        assert!(cell.outputs.is_empty());
        assert_eq!(cell.metadata, serde_json::json!({}));
    }

    #[test]
    fn test_add_cell_full_index_ordering() {
        let mut doc = NotebookDoc::new("nb-order");
        doc.add_cell_full(0, "a", "code", "first", &[], "null", &serde_json::json!({}))
            .unwrap();
        doc.add_cell_full(
            1,
            "b",
            "code",
            "second",
            &[],
            "null",
            &serde_json::json!({}),
        )
        .unwrap();
        doc.add_cell_full(2, "c", "code", "third", &[], "null", &serde_json::json!({}))
            .unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "a");
        assert_eq!(cells[0].source, "first");
        assert_eq!(cells[1].id, "b");
        assert_eq!(cells[1].source, "second");
        assert_eq!(cells[2].id, "c");
        assert_eq!(cells[2].source, "third");
    }

    #[test]
    fn test_clear_all_cells() {
        let mut doc = NotebookDoc::new("nb-clear");
        doc.add_cell(0, "c1", "code").unwrap();
        doc.add_cell(1, "c2", "code").unwrap();
        doc.add_cell(2, "c3", "markdown").unwrap();
        assert_eq!(doc.cell_count(), 3);

        doc.clear_all_cells().unwrap();
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(doc.get_cells(), vec![]);

        // notebook_id metadata should be preserved
        assert_eq!(doc.notebook_id(), Some("nb-clear".to_string()));
    }

    #[test]
    fn test_cell_metadata_read_write() {
        let mut doc = NotebookDoc::new("nb-meta");
        doc.add_cell(0, "cell1", "code").unwrap();

        // New cells should have empty metadata
        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.metadata, serde_json::json!({}));
        assert!(!cell.is_source_hidden());
        assert!(!cell.is_outputs_hidden());
        assert!(cell.tags().is_empty());

        // Set entire metadata
        doc.set_cell_metadata(
            "cell1",
            &serde_json::json!({
                "tags": ["hide-input"],
                "custom_field": "value"
            }),
        )
        .unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.tags(), vec!["hide-input"]);
        assert_eq!(
            cell.metadata.get("custom_field"),
            Some(&serde_json::json!("value"))
        );
    }

    #[test]
    fn test_cell_metadata_typed_setters() {
        let mut doc = NotebookDoc::new("nb-typed");
        doc.add_cell(0, "cell1", "code").unwrap();

        // Set source hidden
        doc.set_cell_source_hidden("cell1", true).unwrap();
        let cell = doc.get_cell("cell1").unwrap();
        assert!(cell.is_source_hidden());
        assert!(!cell.is_outputs_hidden());

        // Set outputs hidden
        doc.set_cell_outputs_hidden("cell1", true).unwrap();
        let cell = doc.get_cell("cell1").unwrap();
        assert!(cell.is_source_hidden());
        assert!(cell.is_outputs_hidden());

        // Set tags
        doc.set_cell_tags("cell1", vec!["test".to_string(), "example".to_string()])
            .unwrap();
        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.tags(), vec!["test", "example"]);

        // Verify structure: jupyter namespace is correct
        assert_eq!(
            cell.metadata.get("jupyter"),
            Some(&serde_json::json!({"source_hidden": true, "outputs_hidden": true}))
        );
    }

    #[test]
    fn test_cell_metadata_path_update() {
        let mut doc = NotebookDoc::new("nb-path");
        doc.add_cell(0, "cell1", "code").unwrap();

        // Update nested path
        doc.update_cell_metadata_at(
            "cell1",
            &["custom", "nested", "value"],
            serde_json::json!(42),
        )
        .unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(
            cell.metadata,
            serde_json::json!({"custom": {"nested": {"value": 42}}})
        );

        // Update another path without clobbering
        doc.update_cell_metadata_at("cell1", &["custom", "other"], serde_json::json!("hello"))
            .unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(
            cell.metadata,
            serde_json::json!({"custom": {"nested": {"value": 42}, "other": "hello"}})
        );
    }

    #[test]
    fn test_cell_metadata_add_cell_full() {
        let mut doc = NotebookDoc::new("nb-full-meta");
        doc.add_cell_full(
            0,
            "cell1",
            "code",
            "print('test')",
            &[],
            "null",
            &serde_json::json!({
                "jupyter": {"source_hidden": true},
                "tags": ["test"]
            }),
        )
        .unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert!(cell.is_source_hidden());
        assert_eq!(cell.tags(), vec!["test"]);
    }

    #[test]
    fn test_cell_metadata_sync() {
        use automerge::sync;

        let mut daemon = NotebookDoc::new("nb-sync-meta");
        daemon.add_cell(0, "cell1", "code").unwrap();
        daemon.set_cell_source_hidden("cell1", true).unwrap();
        daemon
            .set_cell_tags("cell1", vec!["synced".to_string()])
            .unwrap();

        let mut client = NotebookDoc::empty();
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Sync
        for _ in 0..5 {
            if let Some(msg) = daemon.generate_sync_message(&mut daemon_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                daemon.receive_sync_message(&mut daemon_state, msg).unwrap();
            }
        }

        // Verify client has metadata
        let cell = client.get_cell("cell1").unwrap();
        assert!(cell.is_source_hidden());
        assert_eq!(cell.tags(), vec!["synced"]);
    }
}
