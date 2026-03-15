//! WASM bindings for runtimed notebook document operations.
//!
//! Compiled from the same `automerge = "0.7"` crate as the daemon,
//! guaranteeing wire-compatible sync messages. The frontend imports
//! this WASM module instead of `@automerge/automerge` to avoid
//! version mismatch issues that produce phantom cells.

use automerge::sync;
use notebook_doc::presence;
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
    },
    /// A sync message was generated in response; frontend should send it back.
    SyncReply {
        /// The sync message bytes to send to the daemon.
        reply: Vec<u8>,
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
        }
    }

    /// Create a handle with an empty Automerge doc (zero operations) for
    /// sync-only bootstrap.  The sync protocol populates the doc from the
    /// daemon — no `GetDocBytes` needed.
    pub fn create_empty() -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::empty(),
            sync_state: sync::State::new(),
        }
    }

    /// Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
    pub fn load(bytes: &[u8]) -> Result<NotebookHandle, JsError> {
        let doc =
            NotebookDoc::load(bytes).map_err(|e| JsError::new(&format!("load failed: {}", e)))?;
        Ok(NotebookHandle {
            doc,
            sync_state: sync::State::new(),
        })
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

    /// Set a metadata value (legacy string API).
    pub fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), JsError> {
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
        self.doc
            .add_uv_dependency(pkg)
            .map_err(|e| JsError::new(&format!("add_uv_dependency failed: {}", e)))
    }

    /// Remove a UV dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_uv_dependency(&mut self, pkg: &str) -> Result<bool, JsError> {
        self.doc
            .remove_uv_dependency(pkg)
            .map_err(|e| JsError::new(&format!("remove_uv_dependency failed: {}", e)))
    }

    /// Clear the UV section entirely (deps + requires-python).
    pub fn clear_uv_section(&mut self) -> Result<(), JsError> {
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
        self.doc
            .set_uv_requires_python(requires_python)
            .map_err(|e| JsError::new(&format!("set_uv_requires_python failed: {}", e)))
    }

    /// Set UV prerelease strategy, preserving deps and requires-python.
    /// Pass "allow", "disallow", "if-necessary", "explicit", "if-necessary-or-explicit", or null to clear.
    pub fn set_uv_prerelease(&mut self, prerelease: Option<String>) -> Result<(), JsError> {
        self.doc
            .set_uv_prerelease(prerelease)
            .map_err(|e| JsError::new(&format!("set_uv_prerelease failed: {}", e)))
    }

    // ── Conda dependency operations ──────────────────────────────

    /// Add a Conda dependency, deduplicating by package name (case-insensitive).
    /// Initializes the Conda section with ["conda-forge"] channels if absent.
    pub fn add_conda_dependency(&mut self, pkg: &str) -> Result<(), JsError> {
        self.doc
            .add_conda_dependency(pkg)
            .map_err(|e| JsError::new(&format!("add_conda_dependency failed: {}", e)))
    }

    /// Remove a Conda dependency by package name (case-insensitive).
    /// Returns true if a dependency was removed.
    pub fn remove_conda_dependency(&mut self, pkg: &str) -> Result<bool, JsError> {
        self.doc
            .remove_conda_dependency(pkg)
            .map_err(|e| JsError::new(&format!("remove_conda_dependency failed: {}", e)))
    }

    /// Clear the Conda section entirely.
    pub fn clear_conda_section(&mut self) -> Result<(), JsError> {
        self.doc
            .clear_conda_section()
            .map_err(|e| JsError::new(&format!("clear_conda_section failed: {}", e)))
    }

    /// Set Conda channels, preserving deps and python.
    /// Accepts a JSON array string (e.g. `'["conda-forge","bioconda"]'`).
    pub fn set_conda_channels(&mut self, channels_json: &str) -> Result<(), JsError> {
        let channels: Vec<String> = serde_json::from_str(channels_json)
            .map_err(|e| JsError::new(&format!("invalid channels JSON: {}", e)))?;
        self.doc
            .set_conda_channels(channels)
            .map_err(|e| JsError::new(&format!("set_conda_channels failed: {}", e)))
    }

    /// Set Conda python version, preserving deps and channels.
    /// Pass undefined/null to clear the constraint.
    pub fn set_conda_python(&mut self, python: Option<String>) -> Result<(), JsError> {
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

    /// Reset the sync state. Call this when reconnecting to a new daemon session.
    pub fn reset_sync_state(&mut self) {
        self.sync_state = sync::State::new();
    }

    /// Receive a typed frame from the daemon, demux by type byte, return events for the frontend.
    ///
    /// The input is the raw frame bytes from the `notebook:frame` Tauri event:
    /// `[frame_type_byte, ...payload]`.
    ///
    /// Returns a JS array of `FrameEvent` objects directly via `serde-wasm-bindgen`
    /// (no JSON string intermediate). Usually one event, but sync frames may produce
    /// both a `sync_applied` and a `sync_reply` if the local doc needs to send a
    /// response.
    ///
    /// When a `SyncReply` event is returned, its `reply` field contains raw
    /// Automerge sync bytes (no frame type prefix). The frontend must prepend
    /// the frame type byte (`0x00` for AutomergeSync) to form a complete typed
    /// frame, then send it back via `invoke("send_frame", { frameData })`.
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

                events.push(FrameEvent::SyncApplied { changed });

                // The sync protocol may need a reply
                if let Some(reply_msg) = self.doc.generate_sync_message(&mut self.sync_state) {
                    events.push(FrameEvent::SyncReply {
                        reply: reply_msg.encode(),
                    });
                }
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
            _ => {
                events.push(FrameEvent::Unknown { frame_type });
            }
        }

        serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED)
    }
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
