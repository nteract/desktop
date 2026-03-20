//! WASM bindings for runtimed notebook document operations.
//!
//! Compiled from the same `automerge = "0.7"` crate as the daemon,
//! guaranteeing wire-compatible sync messages. The frontend imports
//! this WASM module instead of `@automerge/automerge` to avoid
//! version mismatch issues that produce phantom cells.

use automerge::patches::PatchAction;
use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::Prop;
use notebook_doc::diff::{diff_cells, CellChangeset};
use notebook_doc::presence;
use notebook_doc::runtime_state::{RuntimeState, RuntimeStateDoc};
use notebook_doc::{CellSnapshot, NotebookDoc};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Serialize a Rust value to a `JsValue`, forcing maps to plain JS Objects.
///
/// `serde_wasm_bindgen::to_value` defaults to serializing maps as JS `Map`,
/// but `#[serde(flatten)]` causes serde to emit the containing struct via
/// `serialize_map`. That turns structs like `RuntMetadata` (which flattens
/// an `extra: HashMap`) into JS `Map` objects — breaking dot-access on the
/// JS side (`snapshot.runt.uv` becomes `undefined`).
///
/// Using `serialize_maps_as_objects(true)` ensures all maps become plain
/// JS Objects, matching what `JSON.parse()` would produce. The returned
/// `JsValue` can be any JS type (object, array, scalar) depending on input.
fn serialize_to_js<T: Serialize>(value: &T) -> Result<JsValue, serde_wasm_bindgen::Error> {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    value.serialize(&serializer)
}

use notebook_doc::frame_types;

/// A text attribution range produced when a sync message modifies cell source.
///
/// Pushed to the frontend inside `SyncApplied` so it can highlight freshly
/// arrived text (e.g., a fade-in glow showing who wrote it).
#[derive(Serialize)]
pub struct TextAttribution {
    /// The cell ID whose source was modified.
    pub cell_id: String,
    /// Character index in the source where the change starts.
    pub index: usize,
    /// Text that was inserted at this index (empty for pure deletions).
    pub text: String,
    /// Number of characters deleted at this index (0 for pure insertions).
    pub deleted: usize,
    /// Actor label(s) that contributed to this sync batch.
    pub actors: Vec<String>,
}

/// Event returned from `receive_frame()` for the frontend to handle.
///
/// Converted directly to a JS object via `serde-wasm-bindgen` — no JSON
/// string serialization round-trip. The frontend reads the `type` field
/// to dispatch to the appropriate handler.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrameEvent {
    /// Automerge sync message was applied; frontend should materialize cells.
    SyncApplied {
        /// True if the document changed (new cells, updated source, etc.)
        changed: bool,
        /// Structural changeset describing which cells/fields changed.
        /// `None` when `changed` is false.
        #[serde(skip_serializing_if = "Option::is_none")]
        changeset: Option<CellChangeset>,
        /// Text attribution ranges for source edits in this sync batch.
        /// Empty when `changed` is false or when only non-source fields changed.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attributions: Vec<TextAttribution>,
    },
    /// Broadcast event from the daemon (kernel status, output, etc.)
    Broadcast {
        /// The broadcast payload (parsed from JSON frame, passed through as-is).
        payload: serde_json::Value,
    },
    /// Presence update from a remote peer.
    Presence {
        /// The decoded presence message (decoded from CBOR, passed through as-is).
        payload: serde_json::Value,
    },
    /// Runtime state document was synced — frontend should update runtime state UI.
    RuntimeStateSyncApplied {
        /// True if the runtime state document changed.
        changed: bool,
        /// The full current runtime state snapshot (only when changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<RuntimeState>,
    },
    /// Unknown frame type — frontend can log and ignore.
    Unknown { frame_type: u8 },
}

/// A handle to a local Automerge notebook document.
///
/// All mutations (add cell, delete cell, edit source) happen locally
/// and produce sync messages that the Tauri relay forwards to the daemon.
/// Incoming sync messages from the daemon are applied here, and the
/// frontend re-reads cells to update React state.
#[wasm_bindgen]
pub struct NotebookHandle {
    doc: NotebookDoc,
    sync_state: sync::State,
    /// Runtime state doc — daemon-authoritative, synced read-only.
    state_doc: RuntimeStateDoc,
    state_sync_state: sync::State,
    /// Cached metadata fingerprint — invalidated on `receive_frame` when
    /// the doc changes and on all local metadata mutation methods.
    /// Avoids re-serializing the metadata snapshot on every
    /// `get_metadata_fingerprint()` call (~30/sec during streaming).
    metadata_fingerprint_cache: Option<String>,
}

/// A cell snapshot returned to JavaScript.
#[wasm_bindgen]
pub struct JsCell {
    /// Index in the sorted cell list (for backward compatibility).
    #[wasm_bindgen(readonly)]
    pub index: usize,
    id: String,
    cell_type: String,
    position: String,
    source: String,
    execution_count: String,
    outputs: Vec<String>,
    metadata: serde_json::Value,
    resolved_assets: std::collections::HashMap<String, String>,
}

#[wasm_bindgen]
impl JsCell {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn cell_type(&self) -> String {
        self.cell_type.clone()
    }

    /// Fractional index hex string for ordering (e.g., "80", "7F80").
    #[wasm_bindgen(getter)]
    pub fn position(&self) -> String {
        self.position.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn source(&self) -> String {
        self.source.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn execution_count(&self) -> String {
        self.execution_count.clone()
    }

    /// Get outputs as a JSON array string.
    #[wasm_bindgen(getter)]
    pub fn outputs_json(&self) -> String {
        serde_json::to_string(&self.outputs).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get metadata as a JSON object string.
    #[wasm_bindgen(getter)]
    pub fn metadata_json(&self) -> String {
        serde_json::to_string(&self.metadata).unwrap_or_else(|_| "{}".to_string())
    }

    /// Get resolved asset refs as a JSON object string (`ref` → blob hash).
    #[wasm_bindgen(getter)]
    pub fn resolved_assets_json(&self) -> String {
        serde_json::to_string(&self.resolved_assets).unwrap_or_else(|_| "{}".to_string())
    }
}

impl From<(usize, CellSnapshot)> for JsCell {
    fn from((index, snap): (usize, CellSnapshot)) -> Self {
        JsCell {
            index,
            id: snap.id,
            cell_type: snap.cell_type,
            position: snap.position,
            source: snap.source,
            execution_count: snap.execution_count,
            outputs: snap.outputs,
            metadata: snap.metadata,
            resolved_assets: snap.resolved_assets,
        }
    }
}

#[wasm_bindgen]
impl NotebookHandle {
    /// Create a new empty notebook document.
    #[wasm_bindgen(constructor)]
    pub fn new(notebook_id: &str) -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::new(notebook_id),
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
        }
    }

    /// Create a handle with an empty Automerge doc (zero operations) for
    /// sync-only bootstrap.  The sync protocol populates the doc from the
    /// daemon — no `GetDocBytes` needed.
    pub fn create_empty() -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::empty(),
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
        }
    }

    /// Create an empty sync-only bootstrap handle with a specific actor identity.
    ///
    /// The `actor_label` is a self-attested identity string (e.g., `"human:<session>"`,
    /// `"agent:claude:<session>"`) that tags all subsequent edits for provenance.
    pub fn create_empty_with_actor(actor_label: &str) -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::empty_with_actor(actor_label),
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
        }
    }

    /// Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
    pub fn load(bytes: &[u8]) -> Result<NotebookHandle, JsError> {
        let doc =
            NotebookDoc::load(bytes).map_err(|e| JsError::new(&format!("load failed: {}", e)))?;
        Ok(NotebookHandle {
            doc,
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
        })
    }

    /// Get the actor identity label for this document.
    pub fn get_actor_id(&self) -> String {
        self.doc.get_actor_id()
    }

    /// Set the actor identity for this document.
    ///
    /// Tags all subsequent edits with this label for provenance tracking.
    pub fn set_actor(&mut self, actor_label: &str) {
        self.doc.set_actor(actor_label);
    }

    /// Return the deduplicated, sorted list of actor labels that have
    /// contributed changes to this document's history.
    ///
    /// Useful for debugging provenance — call after sync to see which
    /// peers (e.g., `"runtimed"`, `"human:abc123"`) have touched the notebook.
    pub fn contributing_actors(&mut self) -> Vec<String> {
        self.doc.contributing_actors()
    }

    /// Get the number of cells in the document.
    pub fn cell_count(&self) -> usize {
        self.doc.cell_count()
    }

    /// Get all cells as an array of JsCell objects.
    pub fn get_cells(&self) -> Vec<JsCell> {
        self.doc
            .get_cells()
            .into_iter()
            .enumerate()
            .map(JsCell::from)
            .collect()
    }

    /// Get all cells as a JSON string (for bulk materialization).
    pub fn get_cells_json(&self) -> String {
        let cells = self.doc.get_cells();
        serde_json::to_string(&cells).unwrap_or_else(|_| "[]".to_string())
    }

    // ── Per-cell granular accessors ─────────────────────────────────
    //
    // These avoid full get_cells_json() serialization by crossing the
    // WASM boundary only for the requested data.

    /// Get ordered cell IDs (sorted by position, tiebreak on ID).
    pub fn get_cell_ids(&self) -> Vec<String> {
        self.doc.get_cell_ids()
    }

    /// Get a cell's source text.
    pub fn get_cell_source(&self, cell_id: &str) -> Option<String> {
        self.doc.get_cell_source(cell_id)
    }

    /// Get a cell's type — "code", "markdown", or "raw".
    pub fn get_cell_type(&self, cell_id: &str) -> Option<String> {
        self.doc.get_cell_type(cell_id)
    }

    /// Get a cell's outputs as a native JS array of strings.
    ///
    /// Each element is a JSON-encoded Jupyter output object (or manifest hash).
    /// Returns undefined if the cell doesn't exist.
    pub fn get_cell_outputs(&self, cell_id: &str) -> JsValue {
        match self.doc.get_cell_outputs(cell_id) {
            Some(outputs) => serialize_to_js(&outputs).unwrap_or(JsValue::UNDEFINED),
            None => JsValue::UNDEFINED,
        }
    }

    /// Get a cell's execution count.
    pub fn get_cell_execution_count(&self, cell_id: &str) -> Option<String> {
        self.doc.get_cell_execution_count(cell_id)
    }

    /// Get a cell's metadata as a native JS object.
    ///
    /// Returns undefined if the cell doesn't exist.
    pub fn get_cell_metadata(&self, cell_id: &str) -> JsValue {
        match self.doc.get_cell_metadata(cell_id) {
            Some(metadata) => serialize_to_js(&metadata).unwrap_or(JsValue::UNDEFINED),
            None => JsValue::UNDEFINED,
        }
    }

    /// Get a cell's fractional index position string.
    pub fn get_cell_position(&self, cell_id: &str) -> Option<String> {
        self.doc.get_cell_position(cell_id)
    }

    /// Get a single cell by ID, or null if not found.
    pub fn get_cell(&self, cell_id: &str) -> Option<JsCell> {
        let cells = self.doc.get_cells();
        cells
            .into_iter()
            .enumerate()
            .find(|(_, c)| c.id == cell_id)
            .map(JsCell::from)
    }

    /// Add a new cell at the given index (backward-compatible API).
    ///
    /// Internally converts the index to an after_cell_id for fractional indexing.
    pub fn add_cell(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), JsError> {
        self.doc
            .add_cell(index, cell_id, cell_type)
            .map_err(|e| JsError::new(&format!("add_cell failed: {}", e)))
    }

    /// Add a new cell after the specified cell (semantic API).
    ///
    /// - `after_cell_id = null` → insert at the beginning
    /// - `after_cell_id = "id"` → insert after that cell
    ///
    /// Returns the position string of the new cell.
    pub fn add_cell_after(
        &mut self,
        cell_id: &str,
        cell_type: &str,
        after_cell_id: Option<String>,
    ) -> Result<String, JsError> {
        self.doc
            .add_cell_after(cell_id, cell_type, after_cell_id.as_deref())
            .map_err(|e| JsError::new(&format!("add_cell_after failed: {}", e)))
    }

    /// Move a cell to a new position (after the specified cell).
    ///
    /// - `after_cell_id = null` → move to the beginning
    /// - `after_cell_id = "id"` → move after that cell
    ///
    /// This only updates the cell's position field — no delete/re-insert.
    /// Returns the new position string.
    pub fn move_cell(
        &mut self,
        cell_id: &str,
        after_cell_id: Option<String>,
    ) -> Result<String, JsError> {
        self.doc
            .move_cell(cell_id, after_cell_id.as_deref())
            .map_err(|e| JsError::new(&format!("move_cell failed: {}", e)))
    }

    /// Delete a cell by ID. Returns true if the cell was found and deleted.
    pub fn delete_cell(&mut self, cell_id: &str) -> Result<bool, JsError> {
        self.doc
            .delete_cell(cell_id)
            .map_err(|e| JsError::new(&format!("delete_cell failed: {}", e)))
    }

    /// Update a cell's source text using Automerge Text CRDT (Myers diff).
    pub fn update_source(&mut self, cell_id: &str, source: &str) -> Result<bool, JsError> {
        self.doc
            .update_source(cell_id, source)
            .map_err(|e| JsError::new(&format!("update_source failed: {}", e)))
    }

    /// Splice a cell's source at a specific position (character-level, no diff).
    pub fn splice_source(
        &mut self,
        cell_id: &str,
        index: usize,
        delete_count: usize,
        text: &str,
    ) -> Result<bool, JsError> {
        self.doc
            .splice_source(cell_id, index, delete_count, text)
            .map_err(|e| JsError::new(&format!("splice_source failed: {}", e)))
    }

    /// Clear all outputs from a cell in the CRDT.
    pub fn clear_outputs(&mut self, cell_id: &str) -> Result<bool, JsError> {
        self.doc
            .clear_outputs(cell_id)
            .map_err(|e| JsError::new(&format!("clear_outputs failed: {}", e)))
    }

    /// Set the execution count for a cell. Pass "null" or a number string like "5".
    pub fn set_execution_count(&mut self, cell_id: &str, count: &str) -> Result<bool, JsError> {
        self.doc
            .set_execution_count(cell_id, count)
            .map_err(|e| JsError::new(&format!("set_execution_count failed: {}", e)))
    }

    /// Append text to a cell's source (optimized for streaming, no diff).
    pub fn append_source(&mut self, cell_id: &str, text: &str) -> Result<bool, JsError> {
        self.doc
            .append_source(cell_id, text)
            .map_err(|e| JsError::new(&format!("append_source failed: {}", e)))
    }

    /// Get a metadata value by key (legacy string API).
    pub fn get_metadata(&self, key: &str) -> Option<String> {
        self.doc.get_metadata(key)
    }

    /// Get the full typed metadata as a JSON string.
    ///
    /// Returns the `NotebookMetadataSnapshot` serialized as JSON, or undefined
    /// if no metadata is set. The frontend can parse this with a shared TS interface.
    pub fn get_metadata_snapshot_json(&self) -> Option<String> {
        let snapshot = self.doc.get_metadata_snapshot()?;
        serde_json::to_string(&snapshot).ok()
    }

    /// Get the full typed metadata as a native JS object.
    ///
    /// Returns the `NotebookMetadataSnapshot` as a JS object via serde-wasm-bindgen,
    /// avoiding JSON string round-trips. Returns undefined if no metadata is set.
    pub fn get_metadata_snapshot(&self) -> JsValue {
        match self.doc.get_metadata_snapshot() {
            Some(snapshot) => serialize_to_js(&snapshot).unwrap_or(JsValue::UNDEFINED),
            None => JsValue::UNDEFINED,
        }
    }

    /// Get a metadata value as a native JS value.
    ///
    /// Reads the Automerge metadata subtree and returns it as a JS object/array/scalar.
    /// Returns undefined if the key doesn't exist.
    pub fn get_metadata_value(&self, key: &str) -> JsValue {
        match self.doc.get_metadata_value(key) {
            Some(value) => serialize_to_js(&value).unwrap_or(JsValue::UNDEFINED),
            None => JsValue::UNDEFINED,
        }
    }

    /// Detect the notebook runtime from kernelspec/language_info metadata.
    ///
    /// Returns "python", "deno", or undefined for unknown runtimes.
    pub fn detect_runtime(&self) -> Option<String> {
        self.doc.detect_runtime()
    }

    /// Invalidate the cached metadata fingerprint.
    fn invalidate_metadata_cache(&mut self) {
        self.metadata_fingerprint_cache = None;
    }

    /// Return a stable fingerprint of the notebook metadata.
    ///
    /// Returns a cached JSON string suitable for equality comparison.
    /// The cache is invalidated in `receive_frame` when the Automerge
    /// doc actually changes (heads differ) and on all local metadata
    /// mutation methods.
    ///
    /// Returns undefined if no metadata is present.
    pub fn get_metadata_fingerprint(&mut self) -> Option<String> {
        if let Some(ref cached) = self.metadata_fingerprint_cache {
            return Some(cached.clone());
        }
        let fp = self.doc.get_metadata_fingerprint()?;
        self.metadata_fingerprint_cache = Some(fp.clone());
        Some(fp)
    }

    /// Set a metadata value (legacy string API).
    pub fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .set_metadata(key, value)
            .map_err(|e| JsError::new(&format!("set_metadata failed: {}", e)))
    }

    /// Set the full typed metadata snapshot from a JS object.
    ///
    /// Accepts a JS object matching the `NotebookMetadataSnapshot` shape and writes
    /// it as native Automerge types (maps, lists, scalars). This enables per-field
    /// CRDT merging instead of last-write-wins on a JSON string.
    pub fn set_metadata_snapshot_value(&mut self, value: JsValue) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        let snapshot: notebook_doc::metadata::NotebookMetadataSnapshot =
            serde_wasm_bindgen::from_value(value)
                .map_err(|e| JsError::new(&format!("invalid metadata snapshot: {}", e)))?;
        self.doc
            .set_metadata_snapshot(&snapshot)
            .map_err(|e| JsError::new(&format!("set_metadata_snapshot failed: {}", e)))
    }

    /// Set a metadata value from a JS object (native Automerge types).
    ///
    /// Accepts any JS value and writes it as native Automerge types under the
    /// given key in the metadata map. Objects become Maps, arrays become Lists,
    /// and scalars become native scalars.
    pub fn set_metadata_value(&mut self, key: &str, value: JsValue) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        let json_value: serde_json::Value = serde_wasm_bindgen::from_value(value)
            .map_err(|e| JsError::new(&format!("invalid metadata value: {}", e)))?;
        self.doc
            .set_metadata_value(key, &json_value)
            .map_err(|e| JsError::new(&format!("set_metadata_value failed: {}", e)))
    }

    // ── Cell metadata operations ─────────────────────────────────

    /// Set whether the cell source should be hidden (JupyterLab convention).
    ///
    /// Sets `metadata.jupyter.source_hidden` for the specified cell.
    /// Returns true if the cell was found and updated.
    pub fn set_cell_source_hidden(&mut self, cell_id: &str, hidden: bool) -> Result<bool, JsError> {
        self.doc
            .set_cell_source_hidden(cell_id, hidden)
            .map_err(|e| JsError::new(&format!("set_cell_source_hidden failed: {}", e)))
    }

    /// Set whether the cell outputs should be hidden (JupyterLab convention).
    ///
    /// Sets `metadata.jupyter.outputs_hidden` for the specified cell.
    /// Returns true if the cell was found and updated.
    pub fn set_cell_outputs_hidden(
        &mut self,
        cell_id: &str,
        hidden: bool,
    ) -> Result<bool, JsError> {
        self.doc
            .set_cell_outputs_hidden(cell_id, hidden)
            .map_err(|e| JsError::new(&format!("set_cell_outputs_hidden failed: {}", e)))
    }

    /// Set the cell tags.
    ///
    /// Accepts a JSON array string (e.g. `'["hide-input", "parameters"]'`).
    /// Returns true if the cell was found and updated.
    pub fn set_cell_tags(&mut self, cell_id: &str, tags_json: &str) -> Result<bool, JsError> {
        let tags: Vec<String> = serde_json::from_str(tags_json)
            .map_err(|e| JsError::new(&format!("invalid tags JSON: {}", e)))?;
        self.doc
            .set_cell_tags(cell_id, tags)
            .map_err(|e| JsError::new(&format!("set_cell_tags failed: {}", e)))
    }

    /// Set the cell tags from a JS array (native, no JSON string).
    ///
    /// Accepts a JS array of strings directly via serde-wasm-bindgen.
    pub fn set_cell_tags_value(&mut self, cell_id: &str, tags: JsValue) -> Result<bool, JsError> {
        let tags: Vec<String> = serde_wasm_bindgen::from_value(tags)
            .map_err(|e| JsError::new(&format!("invalid tags value: {}", e)))?;
        self.doc
            .set_cell_tags(cell_id, tags)
            .map_err(|e| JsError::new(&format!("set_cell_tags failed: {}", e)))
    }

    /// Update cell metadata at a specific path (e.g., ["jupyter", "source_hidden"]).
    ///
    /// Creates intermediate objects if they don't exist.
    /// Accepts path and value as JSON strings.
    /// Returns true if the cell was found and updated.
    pub fn update_cell_metadata_at(
        &mut self,
        cell_id: &str,
        path_json: &str,
        value_json: &str,
    ) -> Result<bool, JsError> {
        let path: Vec<String> = serde_json::from_str(path_json)
            .map_err(|e| JsError::new(&format!("invalid path JSON: {}", e)))?;
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsError::new(&format!("invalid value JSON: {}", e)))?;
        let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        self.doc
            .update_cell_metadata_at(cell_id, &path_refs, value)
            .map_err(|e| JsError::new(&format!("update_cell_metadata_at failed: {}", e)))
    }

    /// Update cell metadata at a specific path using native JS values.
    ///
    /// Path is a JS array of strings, value is any JS value.
    /// No JSON string round-trips.
    pub fn update_cell_metadata_at_value(
        &mut self,
        cell_id: &str,
        path: JsValue,
        value: JsValue,
    ) -> Result<bool, JsError> {
        let path: Vec<String> = serde_wasm_bindgen::from_value(path)
            .map_err(|e| JsError::new(&format!("invalid path: {}", e)))?;
        let value: serde_json::Value = serde_wasm_bindgen::from_value(value)
            .map_err(|e| JsError::new(&format!("invalid value: {}", e)))?;
        let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        self.doc
            .update_cell_metadata_at(cell_id, &path_refs, value)
            .map_err(|e| JsError::new(&format!("update_cell_metadata_at failed: {}", e)))
    }

    /// Replace entire cell metadata (last-write-wins).
    ///
    /// Accepts metadata as a JSON object string.
    /// Returns true if the cell was found and updated.
    pub fn set_cell_metadata(
        &mut self,
        cell_id: &str,
        metadata_json: &str,
    ) -> Result<bool, JsError> {
        let metadata: serde_json::Value = serde_json::from_str(metadata_json)
            .map_err(|e| JsError::new(&format!("invalid metadata JSON: {}", e)))?;
        if !metadata.is_object() {
            return Err(JsError::new("metadata must be a JSON object"));
        }
        self.doc
            .set_cell_metadata(cell_id, &metadata)
            .map_err(|e| JsError::new(&format!("set_cell_metadata failed: {}", e)))
    }

    /// Replace entire cell metadata from a JS object (native, no JSON string).
    pub fn set_cell_metadata_value(
        &mut self,
        cell_id: &str,
        metadata: JsValue,
    ) -> Result<bool, JsError> {
        let metadata: serde_json::Value = serde_wasm_bindgen::from_value(metadata)
            .map_err(|e| JsError::new(&format!("invalid metadata: {}", e)))?;
        if !metadata.is_object() {
            return Err(JsError::new("metadata must be an object"));
        }
        self.doc
            .set_cell_metadata(cell_id, &metadata)
            .map_err(|e| JsError::new(&format!("set_cell_metadata failed: {}", e)))
    }

    // ── UV dependency operations ─────────────────────────────────

    /// Add a UV dependency, deduplicating by package name (case-insensitive).
    /// Initializes the UV section if absent, preserving existing fields.
    pub fn add_uv_dependency(&mut self, pkg: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .add_uv_dependency(pkg)
            .map_err(|e| JsError::new(&format!("add_uv_dependency failed: {}", e)))
    }

    /// Remove a UV dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_uv_dependency(&mut self, pkg: &str) -> Result<bool, JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .remove_uv_dependency(pkg)
            .map_err(|e| JsError::new(&format!("remove_uv_dependency failed: {}", e)))
    }

    /// Clear the UV section entirely (deps + requires-python).
    pub fn clear_uv_section(&mut self) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .clear_uv_section()
            .map_err(|e| JsError::new(&format!("clear_uv_section failed: {}", e)))
    }

    /// Set UV requires-python constraint, preserving deps.
    /// Pass undefined/null to clear the constraint.
    pub fn set_uv_requires_python(
        &mut self,
        requires_python: Option<String>,
    ) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .set_uv_requires_python(requires_python)
            .map_err(|e| JsError::new(&format!("set_uv_requires_python failed: {}", e)))
    }

    /// Set UV prerelease strategy, preserving deps and requires-python.
    /// Pass "allow", "disallow", "if-necessary", "explicit", "if-necessary-or-explicit", or null to clear.
    pub fn set_uv_prerelease(&mut self, prerelease: Option<String>) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .set_uv_prerelease(prerelease)
            .map_err(|e| JsError::new(&format!("set_uv_prerelease failed: {}", e)))
    }

    // ── Conda dependency operations ──────────────────────────────

    /// Add a Conda dependency, deduplicating by package name (case-insensitive).
    /// Initializes the Conda section with ["conda-forge"] channels if absent.
    pub fn add_conda_dependency(&mut self, pkg: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .add_conda_dependency(pkg)
            .map_err(|e| JsError::new(&format!("add_conda_dependency failed: {}", e)))
    }

    /// Remove a Conda dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_conda_dependency(&mut self, pkg: &str) -> Result<bool, JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .remove_conda_dependency(pkg)
            .map_err(|e| JsError::new(&format!("remove_conda_dependency failed: {}", e)))
    }

    /// Clear the Conda section entirely.
    pub fn clear_conda_section(&mut self) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .clear_conda_section()
            .map_err(|e| JsError::new(&format!("clear_conda_section failed: {}", e)))
    }

    /// Set Conda channels, preserving deps and python.
    /// Accepts a JSON array string (e.g. `'["conda-forge","bioconda"]'`).
    pub fn set_conda_channels(&mut self, channels_json: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        let channels: Vec<String> = serde_json::from_str(channels_json)
            .map_err(|e| JsError::new(&format!("invalid channels JSON: {}", e)))?;
        self.doc
            .set_conda_channels(channels)
            .map_err(|e| JsError::new(&format!("set_conda_channels failed: {}", e)))
    }

    /// Set Conda python version, preserving deps and channels.
    /// Pass undefined/null to clear the constraint.
    pub fn set_conda_python(&mut self, python: Option<String>) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .set_conda_python(python)
            .map_err(|e| JsError::new(&format!("set_conda_python failed: {}", e)))
    }

    /// Generate a sync message to send to the daemon (via the Tauri relay pipe).
    ///
    /// Returns the message as a byte array, or undefined if already in sync.
    /// The caller should prepend the frame type byte (0x00 for AutomergeSync)
    /// and send via `invoke("send_frame", { frameData })`.
    pub fn generate_sync_message(&mut self) -> Option<Vec<u8>> {
        self.doc
            .generate_sync_message(&mut self.sync_state)
            .map(|msg| msg.encode())
    }

    /// Generate a sync reply after one or more inbound frames have been applied.
    ///
    /// This is the same operation as `generate_sync_message()` but named to
    /// communicate the intended usage: the frontend should call this on a
    /// debounce timer after processing inbound sync frames, rather than
    /// replying to every frame individually.
    ///
    /// Safe to call after multiple `receive_frame()` calls — each receive
    /// applies changes cumulatively, and one generate covers everything.
    /// The Automerge sync protocol converges regardless of reply timing.
    pub fn generate_sync_reply(&mut self) -> Option<Vec<u8>> {
        self.doc
            .generate_sync_message(&mut self.sync_state)
            .map(|msg| msg.encode())
    }

    /// Receive and apply a sync message from the daemon (via the Tauri relay pipe).
    ///
    /// Returns true if the document changed (caller should re-read cells).
    pub fn receive_sync_message(&mut self, message: &[u8]) -> Result<bool, JsError> {
        let msg = sync::Message::decode(message)
            .map_err(|e| JsError::new(&format!("decode sync message: {}", e)))?;

        // Compare document heads before and after to detect changes.
        // This is O(number of heads) — far cheaper than the previous approach
        // which called doc.save() twice (serializing the entire document).
        let heads_before = self.doc.doc_mut().get_heads();

        self.doc
            .receive_sync_message(&mut self.sync_state, msg)
            .map_err(|e| JsError::new(&format!("receive sync message: {}", e)))?;

        let heads_after = self.doc.doc_mut().get_heads();
        Ok(heads_before != heads_after)
    }

    /// Export the full document as bytes (for debugging or persistence).
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Generate a sync reply for the RuntimeStateDoc.
    /// Called immediately after each `RuntimeStateSyncApplied` event
    /// so the daemon knows which state the client has received.
    pub fn generate_runtime_state_sync_reply(&mut self) -> Option<Vec<u8>> {
        self.state_doc
            .generate_sync_message(&mut self.state_sync_state)
            .map(|msg| msg.encode())
    }

    /// Read the current runtime state snapshot from the WASM doc.
    pub fn get_runtime_state(&self) -> JsValue {
        let state = self.state_doc.read_state();
        serialize_to_js(&state).unwrap_or(JsValue::UNDEFINED)
    }

    /// Reset the sync state. Call this when reconnecting to a new daemon session.
    pub fn reset_sync_state(&mut self) {
        self.sync_state = sync::State::new();
        self.state_sync_state = sync::State::new();
    }

    /// Receive a typed frame from the daemon, demux by type byte, return events for the frontend.
    ///
    /// The input is the raw frame bytes from the `notebook:frame` Tauri event:
    /// `[frame_type_byte, ...payload]`.
    ///
    /// Returns a JS array of `FrameEvent` objects directly via `serde-wasm-bindgen`
    /// (no JSON string intermediate). Sync frames return a single `sync_applied`
    /// event with an optional `CellChangeset`.
    ///
    /// **Sync replies are NOT generated here.** The frontend must call
    /// `generate_sync_reply()` on a debounce timer to send replies back to the
    /// daemon. This avoids an IPC-per-frame amplification loop — multiple
    /// inbound frames coalesce into a single outbound reply.
    ///
    /// Returns `undefined` if the frame is empty or cannot be processed.
    pub fn receive_frame(&mut self, frame_bytes: &[u8]) -> JsValue {
        if frame_bytes.is_empty() {
            return JsValue::UNDEFINED;
        }

        let frame_type = frame_bytes[0];
        let payload = &frame_bytes[1..];

        let mut events: Vec<FrameEvent> = Vec::new();

        match frame_type {
            frame_types::AUTOMERGE_SYNC => {
                // Decode and apply the sync message to our local doc
                let Ok(msg) = sync::Message::decode(payload) else {
                    return JsValue::UNDEFINED;
                };
                let heads_before = self.doc.doc_mut().get_heads();
                if self
                    .doc
                    .receive_sync_message(&mut self.sync_state, msg)
                    .is_err()
                {
                    return JsValue::UNDEFINED;
                }
                let heads_after = self.doc.doc_mut().get_heads();
                let changed = heads_before != heads_after;

                if changed {
                    // Invalidate cached fingerprint — metadata may have changed.
                    // TODO: This invalidates on ANY doc change (including cell/output
                    // streaming), which forces re-serialization ~30/sec during active
                    // execution. The frontend's fingerprint comparison still saves the
                    // expensive work (snapshot deserialization + subscriber notifications),
                    // but ideally we'd only invalidate when non-cell patches exist.
                    // Options: extend CellChangeset with a `metadata_changed` flag,
                    // or compare the new fingerprint to the cached one here and keep
                    // the cache if metadata didn't actually change.
                    self.metadata_fingerprint_cache = None;
                }

                let (changeset, attributions) = if changed {
                    let cs = diff_cells(self.doc.doc_mut(), &heads_before, &heads_after);
                    let attrs =
                        compute_text_attributions(self.doc.doc_mut(), &heads_before, &heads_after);
                    (Some(cs), attrs)
                } else {
                    (None, Vec::new())
                };

                events.push(FrameEvent::SyncApplied {
                    changed,
                    changeset,
                    attributions,
                });
            }
            frame_types::BROADCAST => {
                // Parse JSON broadcast payload
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
                    return JsValue::UNDEFINED;
                };
                events.push(FrameEvent::Broadcast { payload: value });
            }
            frame_types::PRESENCE => {
                // Decode CBOR presence and convert to JSON value for the frontend
                let Ok(msg) = presence::decode_message(payload) else {
                    return JsValue::UNDEFINED;
                };
                let Ok(value) = serde_json::to_value(&msg) else {
                    return JsValue::UNDEFINED;
                };
                events.push(FrameEvent::Presence { payload: value });
            }
            frame_types::RUNTIME_STATE_SYNC => {
                // Apply daemon's RuntimeStateDoc sync message to our local replica.
                // We use the raw Automerge sync (no change stripping) because the
                // WASM is a read-only consumer — stripping is done daemon-side for
                // the client→daemon direction.
                let Ok(msg) = sync::Message::decode(payload) else {
                    return JsValue::UNDEFINED;
                };
                let heads_before = self.state_doc.doc_mut().get_heads();
                if self
                    .state_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut self.state_sync_state, msg)
                    .is_err()
                {
                    return JsValue::UNDEFINED;
                }
                let heads_after = self.state_doc.doc_mut().get_heads();
                let changed = heads_before != heads_after;

                let state = if changed {
                    Some(self.state_doc.read_state())
                } else {
                    None
                };

                events.push(FrameEvent::RuntimeStateSyncApplied { changed, state });
            }
            _ => {
                events.push(FrameEvent::Unknown { frame_type });
            }
        }

        serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED)
    }
}

// ── Attribution extraction ───────────────────────────────────────────

/// Compute text attribution ranges from the diff between two document states.
///
/// Walks the Automerge patches produced by `diff(before, after)` and extracts
/// `SpliceText` and `DeleteSeq` actions on cell source `Text` objects.
/// The actors are determined from the new changes in the diff range.
///
/// Performance: `diff()` and `get_changes()` only examine the delta — they
/// do not walk the entire document.  The cost is proportional to the number
/// of operations in the new changes, which is typically small per sync cycle.
fn compute_text_attributions(
    doc: &mut automerge::AutoCommit,
    before: &[automerge::ChangeHash],
    after: &[automerge::ChangeHash],
) -> Vec<TextAttribution> {
    use std::collections::BTreeSet;

    // Determine which actors contributed the new changes
    let new_changes = doc.get_changes(before);
    let actors: Vec<String> = new_changes
        .iter()
        .map(|c| notebook_doc::actor_label_from_id(c.actor_id()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if actors.is_empty() {
        return Vec::new();
    }

    // Compute the structural diff — only the delta, not the whole doc
    let patches = doc.diff(before, after);

    let mut result = Vec::new();
    for patch in &patches {
        // Extract cell_id from the patch path.
        //
        // For a source text splice the path looks like:
        //   [(ROOT, "cells"), (cells_map, "<cell-id>"), (cell_obj, "source")]
        //
        // We need at least 2 path elements, and the second must be a Map
        // key (the cell ID).  The last element should be "source".
        let cell_id = match extract_cell_source_id(&patch.path) {
            Some(id) => id,
            None => continue,
        };

        match &patch.action {
            PatchAction::SpliceText { index, value, .. } => {
                let text = value.make_string();
                if !text.is_empty() {
                    result.push(TextAttribution {
                        cell_id: cell_id.clone(),
                        index: *index,
                        text,
                        deleted: 0,
                        actors: actors.clone(),
                    });
                }
            }
            PatchAction::DeleteSeq { index, length } => {
                result.push(TextAttribution {
                    cell_id: cell_id.clone(),
                    index: *index,
                    text: String::new(),
                    deleted: *length,
                    actors: actors.clone(),
                });
            }
            _ => {}
        }
    }
    result
}

/// Extract the cell ID from a patch path if it points to a cell's source text.
///
/// Returns `Some(cell_id)` when the path contains the 3-element window:
///   `[..., (_, "cells"), (_, <cell_id>), (_, "source")]`
///
/// Scans backwards for this exact sequence so we don't match unrelated
/// paths that happen to end in `"source"` (e.g., nested metadata maps).
fn extract_cell_source_id(path: &[(automerge::ObjId, Prop)]) -> Option<String> {
    // Scan backwards for a 3-element window: "cells" → <cell_id> → "source"
    if path.len() < 3 {
        return None;
    }

    for window in path.windows(3).rev() {
        let is_cells = matches!(&window[0].1, Prop::Map(k) if k == "cells");
        let is_source = matches!(&window[2].1, Prop::Map(k) if k == "source");

        if is_cells && is_source {
            if let Prop::Map(cell_id) = &window[1].1 {
                return Some(cell_id.clone());
            }
        }
    }

    None
}

// ── Presence encoding (free functions for wasm_bindgen export) ────────

/// Encode a cursor position as a presence frame payload (CBOR).
///
/// The frontend should prepend the frame type byte (0x04) and send
/// via `invoke("send_frame", { frameData })`.
#[wasm_bindgen]
pub fn encode_cursor_presence(peer_id: &str, cell_id: &str, line: u32, column: u32) -> Vec<u8> {
    presence::encode_cursor_update(
        peer_id,
        &presence::CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    )
}

/// Encode a selection range as a presence frame payload (CBOR).
#[wasm_bindgen]
pub fn encode_selection_presence(
    peer_id: &str,
    cell_id: &str,
    anchor_line: u32,
    anchor_col: u32,
    head_line: u32,
    head_col: u32,
) -> Vec<u8> {
    presence::encode_selection_update(
        peer_id,
        &presence::SelectionRange {
            cell_id: cell_id.to_string(),
            anchor_line,
            anchor_col,
            head_line,
            head_col,
        },
    )
}

/// Encode a cell focus as a presence frame payload (CBOR).
/// Focus means "I'm on this cell" without an editor cursor position.
#[wasm_bindgen]
pub fn encode_focus_presence(peer_id: &str, cell_id: &str) -> Vec<u8> {
    presence::encode_focus_update(peer_id, cell_id)
}

/// Encode a clear-channel message as a presence frame payload (CBOR).
/// Removes a single presence channel (e.g. cursor or selection) for this peer.
#[wasm_bindgen]
pub fn encode_clear_channel_presence(peer_id: &str, channel: &str) -> Vec<u8> {
    let ch = match channel {
        "cursor" => presence::Channel::Cursor,
        "selection" => presence::Channel::Selection,
        "focus" => presence::Channel::Focus,
        _ => return vec![],
    };
    presence::encode_clear_channel(peer_id, ch)
}
