//! WASM bindings for runtimed notebook document operations.
//!
//! Compiled from the same `automerge = "0.7"` crate as the daemon,
//! guaranteeing wire-compatible sync messages. The frontend imports
//! this WASM module instead of `@automerge/automerge` to avoid
//! version mismatch issues that produce phantom cells.

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use automerge::sync;
use automerge::sync::SyncDoc;
use notebook_doc::diff::{diff_doc, CellChangeset, TextPatch};
use notebook_doc::mime::{is_binary_mime, ResolvedContentRef};
use notebook_doc::pool_state::{PoolDoc, PoolState};
use notebook_doc::presence;
use notebook_doc::runtime_state::{diff_execution_outputs, RuntimeState, RuntimeStateDoc};
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
        /// Sync reply to send back to the daemon, generated atomically after
        /// applying the inbound message. `None` when the protocol has nothing
        /// to send (e.g. already in sync). The caller should prepend frame type
        /// byte 0x00 and send via `sendFrame`.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Vec<u8>>,
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
        /// Includes outputs inline in each ExecutionState entry.
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<Box<RuntimeState>>,
        /// Cell IDs whose outputs changed in this sync frame.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        output_changed_cells: Vec<String>,
    },
    /// Sync error recovered — doc rebuilt and sync state normalized.
    ///
    /// Emitted when `receive_sync_message` fails (e.g. automerge MissingOps
    /// bug). The WASM layer automatically rebuilds the doc via save→load and
    /// normalizes sync state via encode→decode round-trip (preserving
    /// `shared_heads`, clearing transient state). The optional `reply`
    /// contains a fresh sync message to restart negotiation.
    SyncError {
        /// True if the document advanced before the error (partial apply).
        /// When true, the SyncEngine should trigger a full materialization
        /// so the UI reflects the recovered doc state.
        changed: bool,
        /// Fresh sync message generated after recovery.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Vec<u8>>,
    },
    /// Runtime state sync error recovered — state doc rebuilt.
    RuntimeStateSyncError {
        /// True if the state doc advanced before the error.
        changed: bool,
        /// The current runtime state snapshot (only when changed).
        /// Included so the SyncEngine can update the UI even if no
        /// further runtime-state frames arrive after recovery.
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<Box<RuntimeState>>,
        /// Fresh sync message generated after recovery.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Vec<u8>>,
    },
    /// Pool state document was synced — frontend should update pool state UI.
    PoolStateSyncApplied {
        /// True if the pool state document changed.
        changed: bool,
        /// The full current pool state snapshot (only when changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<Box<PoolState>>,
    },
    /// Pool state sync error recovered — pool doc rebuilt.
    PoolStateSyncError {
        /// True if the pool doc advanced before the error.
        changed: bool,
        /// The current pool state snapshot (only when changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<Box<PoolState>>,
        /// Fresh sync message generated after recovery.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<Vec<u8>>,
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
    /// Previous outputs per execution_id for diffing. Extracted from
    /// `state.executions[eid].outputs` on each sync frame to detect
    /// mid-execution output changes (stream append, display update, error).
    prev_execution_outputs: std::collections::HashMap<String, Vec<serde_json::Value>>,
    /// Pool state doc — daemon-authoritative, global, synced read-only.
    pool_doc: PoolDoc,
    pool_sync_state: sync::State,
    /// Cached metadata fingerprint — invalidated on `receive_frame` when
    /// the doc changes and on all local metadata mutation methods.
    /// Avoids re-serializing the metadata snapshot on every
    /// `get_metadata_fingerprint()` call (~30/sec during streaming).
    metadata_fingerprint_cache: Option<String>,
    /// MIME type priority list for output selection.
    /// Types earlier in the list are preferred when narrowing output data bundles.
    /// If empty, all MIME types are returned (backward compatible).
    mime_priority: Vec<String>,
    /// Blob server port for resolving binary ContentRefs to URLs.
    /// Set via `set_blob_port()`. When None, binary refs pass through as-is.
    blob_port: Option<u16>,
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
    outputs: Vec<serde_json::Value>,
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

    /// Get outputs as a JSON array string of structured manifest objects.
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

/// Build a `JsCell` from a `CellSnapshot` and the cell's outputs fetched
/// separately from `RuntimeStateDoc`. Outputs no longer travel on
/// `CellSnapshot` — callers resolve them explicitly via
/// `state_doc.get_outputs(execution_id)`.
fn js_cell_from_parts(index: usize, snap: CellSnapshot, outputs: Vec<serde_json::Value>) -> JsCell {
    JsCell {
        index,
        id: snap.id,
        cell_type: snap.cell_type,
        position: snap.position,
        source: snap.source,
        execution_count: snap.execution_count,
        outputs,
        metadata: snap.metadata,
        resolved_assets: snap.resolved_assets,
    }
}

// ── Comm state ContentRef resolution ─────────────────────────────────

/// Recursively walk a JSON value, resolving ContentRef objects in place.
///
/// - `{"inline": V}` → unwrap to `V`
/// - `{"blob": H, "size": N, "media_type": M?}` → plain URL string. The JSON
///   path is recorded in either `buffer_paths` (binary MIME, caller should
///   fetch as ArrayBuffer) or `text_paths` (text MIME, caller should fetch
///   the URL and substitute the decoded text back into the state tree before
///   handing it to widget code). A missing or unknown `media_type` is treated
///   as binary so the value stays a URL — matches the legacy behavior for
///   comms that don't carry MIME metadata.
/// - Arrays/objects → recurse
/// - Primitives → pass through
fn walk_and_resolve_comm_state(
    val: &serde_json::Value,
    port: u16,
    current_path: &mut Vec<String>,
    buffer_paths: &mut Vec<Vec<String>>,
    text_paths: &mut Vec<Vec<String>>,
) -> serde_json::Value {
    match val {
        serde_json::Value::Object(obj) => {
            // Check for inline ContentRef: {"inline": ...}
            if let Some(inner) = obj.get("inline") {
                return inner.clone();
            }

            // Check for blob ContentRef: {"blob": string, "size": number}
            if let (Some(serde_json::Value::String(hash)), Some(serde_json::Value::Number(_))) =
                (obj.get("blob"), obj.get("size"))
            {
                // Anywidget reserves `_esm` and `_css` for URL-preferring
                // loaders: `_esm` flows through `import(url)` and `_css`
                // through `<link rel=stylesheet href=url>`, both of which
                // handle URLs natively. Pulling those blobs over HTTP
                // in the sync engine just to re-serve the decoded string
                // is wasted work and defeats browser caching, so keep
                // them on the URL path regardless of media_type.
                let last_key = current_path.last().map(String::as_str);
                let url_preferred = matches!(last_key, Some("_esm") | Some("_css"));

                let media_type = obj.get("media_type").and_then(|v| v.as_str());
                // Only classify as text when we have a media_type that says so.
                // Missing media_type → binary (URL) to preserve legacy behavior.
                let is_text =
                    !url_preferred && media_type.map(|mt| !is_binary_mime(mt)).unwrap_or(false);
                if is_text {
                    text_paths.push(current_path.clone());
                } else {
                    buffer_paths.push(current_path.clone());
                }
                return serde_json::Value::String(format!(
                    "http://127.0.0.1:{}/blob/{}",
                    port, hash
                ));
            }

            // Regular object — recurse into values
            let mut resolved = serde_json::Map::with_capacity(obj.len());
            for (key, child) in obj {
                current_path.push(key.clone());
                resolved.insert(
                    key.clone(),
                    walk_and_resolve_comm_state(
                        child,
                        port,
                        current_path,
                        buffer_paths,
                        text_paths,
                    ),
                );
                current_path.pop();
            }
            serde_json::Value::Object(resolved)
        }
        serde_json::Value::Array(arr) => {
            let resolved: Vec<serde_json::Value> = arr
                .iter()
                .enumerate()
                .map(|(i, child)| {
                    current_path.push(i.to_string());
                    let r = walk_and_resolve_comm_state(
                        child,
                        port,
                        current_path,
                        buffer_paths,
                        text_paths,
                    );
                    current_path.pop();
                    r
                })
                .collect();
            serde_json::Value::Array(resolved)
        }
        // Primitives pass through unchanged
        _ => val.clone(),
    }
}

#[wasm_bindgen]
impl NotebookHandle {
    /// Create a new empty notebook document.
    #[wasm_bindgen(constructor)]
    pub fn new(notebook_id: &str) -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::new_with_encoding(
                notebook_id,
                notebook_doc::TextEncoding::Utf16CodeUnit,
            ),
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            prev_execution_outputs: std::collections::HashMap::new(),
            pool_doc: PoolDoc::new_empty(),
            pool_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
            mime_priority: Vec::new(),
            blob_port: None,
        }
    }

    /// Create a bootstrap handle for sync — no notebook ID, just skeleton + encoding + actor.
    ///
    /// This is the preferred constructor for sync-only clients. The daemon
    /// populates the full document via Automerge sync.
    pub fn create_bootstrap(actor_label: &str) -> NotebookHandle {
        NotebookHandle {
            doc: NotebookDoc::bootstrap(notebook_doc::TextEncoding::Utf16CodeUnit, actor_label),
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            prev_execution_outputs: std::collections::HashMap::new(),
            pool_doc: PoolDoc::new_empty(),
            pool_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
            mime_priority: Vec::new(),
            blob_port: None,
        }
    }

    /// Create a handle with the bootstrap skeleton for sync.
    ///
    /// Deprecated — use [`create_bootstrap()`](Self::create_bootstrap) which
    /// requires an actor label.
    pub fn create_empty() -> NotebookHandle {
        Self::create_bootstrap("anonymous")
    }

    /// Create a bootstrap handle with a specific actor identity.
    ///
    /// Deprecated — use [`create_bootstrap()`](Self::create_bootstrap).
    pub fn create_empty_with_actor(actor_label: &str) -> NotebookHandle {
        Self::create_bootstrap(actor_label)
    }

    /// Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
    pub fn load(bytes: &[u8]) -> Result<NotebookHandle, JsError> {
        let doc = NotebookDoc::load_with_encoding(bytes, notebook_doc::TextEncoding::Utf16CodeUnit)
            .map_err(|e| JsError::new(&format!("load failed: {}", e)))?;
        Ok(NotebookHandle {
            doc,
            sync_state: sync::State::new(),
            state_doc: RuntimeStateDoc::new_empty(),
            state_sync_state: sync::State::new(),
            prev_execution_outputs: std::collections::HashMap::new(),
            pool_doc: PoolDoc::new_empty(),
            pool_sync_state: sync::State::new(),
            metadata_fingerprint_cache: None,
            mime_priority: Vec::new(),
            blob_port: None,
        })
    }

    /// Load a RuntimeStateDoc from saved bytes.
    ///
    /// Used by test fixtures to provide pre-populated state doc data
    /// (outputs, executions) alongside the notebook doc.
    pub fn load_state_doc(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        let doc = automerge::AutoCommit::load(bytes)
            .map_err(|e| JsError::new(&format!("load_state_doc failed: {}", e)))?;
        self.state_doc = RuntimeStateDoc::from_doc(doc);
        Ok(())
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
    ///
    /// Outputs are fetched from `RuntimeStateDoc` keyed by each cell's
    /// `execution_id`. Cells without an execution_id or with empty outputs
    /// return an empty outputs vec.
    pub fn get_cells(&self) -> Vec<JsCell> {
        self.doc
            .get_cells()
            .into_iter()
            .enumerate()
            .map(|(index, snap)| {
                let outputs = self.fetch_and_narrow_outputs(&snap.id);
                js_cell_from_parts(index, snap, outputs)
            })
            .collect()
    }

    /// Get all cells as a JSON string (for bulk materialization).
    ///
    /// Serializes the same shape as `get_cells()` but as a single JSON
    /// string — cheaper to cross the WASM boundary than many individual
    /// property getters. Outputs are fetched from `RuntimeStateDoc` keyed
    /// by each cell's `execution_id`; `CellSnapshot` itself no longer
    /// carries outputs.
    pub fn get_cells_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct CellWithOutputs<'a> {
            #[serde(flatten)]
            snapshot: &'a CellSnapshot,
            outputs: Vec<serde_json::Value>,
        }

        let cells = self.doc.get_cells();
        let combined: Vec<CellWithOutputs<'_>> = cells
            .iter()
            .map(|snap| {
                let outputs = self.fetch_and_narrow_outputs(&snap.id);
                CellWithOutputs {
                    snapshot: snap,
                    outputs,
                }
            })
            .collect();
        serde_json::to_string(&combined).unwrap_or_else(|_| "[]".to_string())
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

    /// Get a cell's outputs as a native JS array of manifest objects.
    ///
    /// Each element is a structured output manifest (with MIME bundles and
    /// ContentRef blob/inline refs). Returns undefined if the cell doesn't exist.
    ///
    /// Outputs live in the RuntimeStateDoc keyed by execution_id. This method
    /// reads the cell's `execution_id` from the notebook doc, then looks up
    /// outputs in the state doc — the dedicated outputs lookup that replaces
    /// the old "read the snapshot and inspect `snapshot.outputs`" path.
    pub fn get_cell_outputs(&self, cell_id: &str) -> JsValue {
        let outputs = self.fetch_and_narrow_outputs(cell_id);
        if outputs.is_empty() {
            JsValue::UNDEFINED
        } else {
            serialize_to_js(&outputs).unwrap_or(JsValue::UNDEFINED)
        }
    }

    /// Set the MIME type priority list for output selection.
    /// Types earlier in the list are preferred when narrowing output data bundles.
    /// If empty, all MIME types are returned (backward compatible).
    pub fn set_mime_priority(&mut self, priority: JsValue) {
        if let Ok(p) = serde_wasm_bindgen::from_value::<Vec<String>>(priority) {
            self.mime_priority = p;
        }
    }

    /// Set the blob server port for resolving binary ContentRefs to URLs.
    /// Call after init and whenever the daemon restarts with a new port.
    pub fn set_blob_port(&mut self, port: u16) {
        self.blob_port = Some(port);
    }

    /// Fetch a cell's outputs from `RuntimeStateDoc` via its `execution_id`
    /// and narrow the data bundles to the active MIME priority set.
    ///
    /// Returns an empty vec when the cell has no `execution_id` or the
    /// state doc has no outputs for that id. This is the canonical path
    /// for all output reads on the WASM side — `CellSnapshot` no longer
    /// carries outputs.
    fn fetch_and_narrow_outputs(&self, cell_id: &str) -> Vec<serde_json::Value> {
        let Some(eid) = self.doc.get_execution_id(cell_id) else {
            return Vec::new();
        };
        self.state_doc
            .get_outputs(&eid)
            .into_iter()
            .map(|o| self.narrow_output_data(o))
            .collect()
    }

    /// Narrow an output manifest's data bundle to the winning MIME type,
    /// plus all binary MIME refs and text/plain as a fallback candidate.
    ///
    /// Resolves ContentRefs into `ResolvedContentRef` variants:
    /// - Binary MIME types → `Url` (blob server URL, zero fetch cost)
    /// - Inline refs → `Inline` (embedded text)
    /// - Text blob refs → `Blob` (needs JS-side HTTP fetch)
    ///
    /// Only expensive text blob refs for non-winning types are dropped.
    /// Returns the manifest unchanged if mime_priority is empty or output_type
    /// is not display_data/execute_result.
    fn narrow_output_data(&self, mut output: serde_json::Value) -> serde_json::Value {
        if self.mime_priority.is_empty() {
            return output;
        }
        let output_type = output.get("output_type").and_then(|v| v.as_str());
        if !matches!(output_type, Some("display_data" | "execute_result")) {
            return output;
        }
        if let Some(data) = output.get("data").and_then(|d| d.as_object()) {
            let keys: Vec<&str> = data.keys().map(|k| k.as_str()).collect();
            // Find the winning MIME type from priority list
            let winner = self
                .mime_priority
                .iter()
                .find(|p| keys.contains(&p.as_str()))
                .map(|s| s.as_str())
                // Fallback: first available key
                .or_else(|| keys.first().copied());

            if let Some(winner_mime) = winner {
                let mut narrowed = serde_json::Map::new();
                for (mime, val) in data {
                    if mime == winner_mime || mime == "text/plain" || is_binary_mime(mime) {
                        let resolved = self.resolve_content_ref(mime, val);
                        narrowed.insert(mime.clone(), resolved);
                    }
                }
                output["data"] = serde_json::Value::Object(narrowed);
            }
        }
        output
    }

    /// Resolve a ContentRef value into a ResolvedContentRef based on MIME type.
    ///
    /// - `{ "inline": "..." }` → `ResolvedContentRef::Inline`
    /// - Binary MIME + `{ "blob": "hash", "size": N }` → `ResolvedContentRef::Url`
    ///   (when blob_port is set)
    /// - Text MIME + `{ "blob": "hash", "size": N }` → `ResolvedContentRef::Blob`
    fn resolve_content_ref(&self, mime: &str, val: &serde_json::Value) -> serde_json::Value {
        // Inline refs pass through as Inline variant
        if let Some(inline) = val.get("inline").and_then(|v| v.as_str()) {
            return serde_json::to_value(ResolvedContentRef::Inline {
                inline: inline.to_string(),
            })
            .unwrap_or_else(|_| val.clone());
        }

        // Blob refs: resolve binary to URL, leave text as Blob
        if let Some(blob_hash) = val.get("blob").and_then(|v| v.as_str()) {
            let size = val.get("size").and_then(|v| v.as_u64()).unwrap_or(0);

            if is_binary_mime(mime) {
                if let Some(port) = self.blob_port {
                    return serde_json::to_value(ResolvedContentRef::Url {
                        url: format!("http://127.0.0.1:{}/blob/{}", port, blob_hash),
                    })
                    .unwrap_or_else(|_| val.clone());
                }
            }

            return serde_json::to_value(ResolvedContentRef::Blob {
                blob: blob_hash.to_string(),
                size,
            })
            .unwrap_or_else(|_| val.clone());
        }

        // Unknown shape — pass through unchanged
        val.clone()
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
            .map(|(index, snap)| {
                let outputs = self.fetch_and_narrow_outputs(&snap.id);
                js_cell_from_parts(index, snap, outputs)
            })
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

    // ── Pixi dependency operations ──────────────────────────────────

    /// Add a Pixi conda dependency (matchspec). Deduplicates by package name.
    pub fn add_pixi_dependency(&mut self, pkg: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .add_pixi_dependency(pkg)
            .map_err(|e| JsError::new(&format!("add_pixi_dependency failed: {}", e)))
    }

    /// Remove a Pixi conda dependency by package name.
    /// Returns true if a dependency was removed.
    pub fn remove_pixi_dependency(&mut self, pkg: &str) -> Result<bool, JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .remove_pixi_dependency(pkg)
            .map_err(|e| JsError::new(&format!("remove_pixi_dependency failed: {}", e)))
    }

    /// Clear the Pixi section entirely.
    pub fn clear_pixi_section(&mut self) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .clear_pixi_section()
            .map_err(|e| JsError::new(&format!("clear_pixi_section failed: {}", e)))
    }

    /// Set Pixi channels.
    /// Accepts a JSON array string (e.g. `'["conda-forge"]'`).
    pub fn set_pixi_channels(&mut self, channels_json: &str) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        let channels: Vec<String> = serde_json::from_str(channels_json)
            .map_err(|e| JsError::new(&format!("invalid channels JSON: {}", e)))?;
        self.doc
            .set_pixi_channels(channels)
            .map_err(|e| JsError::new(&format!("set_pixi_channels failed: {}", e)))
    }

    /// Set Pixi python version.
    /// Pass undefined/null to clear the constraint.
    pub fn set_pixi_python(&mut self, python: Option<String>) -> Result<(), JsError> {
        self.invalidate_metadata_cache();
        self.doc
            .set_pixi_python(python)
            .map_err(|e| JsError::new(&format!("set_pixi_python failed: {}", e)))
    }

    /// Flush any pending local changes as a sync message to send to the daemon.
    ///
    /// Call this after local CRDT mutations (cell edits, metadata changes) to
    /// push them to the daemon. Returns the message as a byte array, or
    /// `undefined` if there are no unsent local changes.
    ///
    /// This is the ONLY way to generate an outbound sync message besides the
    /// reply embedded in `receive_frame()`. Having exactly two controlled paths
    /// (reply-to-inbound and flush-local) prevents the consumption race from
    /// #1067 where `flushSync` and `syncReply$` both called
    /// `generate_sync_message`, racing on the shared `sync_state`.
    ///
    /// If the returned message cannot be delivered, the caller MUST call
    /// `cancel_last_flush()` to prevent `sent_hashes` from permanently
    /// filtering out the undelivered change data.
    pub fn flush_local_changes(&mut self) -> Option<Vec<u8>> {
        self.doc
            .generate_sync_message(&mut self.sync_state)
            .map(|msg| msg.encode())
    }

    /// Roll back sync state after a failed `flush_local_changes()` delivery.
    ///
    /// If the message from `flush_local_changes()` was NOT delivered to the
    /// daemon (e.g. sendFrame failed, relay mutex blocked), call this to clear
    /// `in_flight` and `sent_hashes`. Without this, `generate_sync_message`
    /// will permanently filter out the change data for hashes it believes were
    /// already sent, causing a protocol stall that only `reset_sync_state()`
    /// (page reload) can recover from.
    ///
    /// Clearing `sent_hashes` may cause some change data to be resent on the
    /// next sync message, but the protocol tolerates duplicates — Automerge's
    /// `load_incremental` deduplicates on receive.
    pub fn cancel_last_flush(&mut self) {
        self.sync_state.in_flight = false;
        self.sync_state.sent_hashes.clear();
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

    /// Set a single property in a comm's state map.
    ///
    /// Writes directly to `comms/{comm_id}/state/{key}` as a native
    /// Automerge value. Call `flush_runtime_state_sync()` after mutations
    /// to propagate changes to the daemon.
    pub fn set_comm_state_property(&mut self, comm_id: &str, key: &str, value_json: &str) -> bool {
        let value: serde_json::Value = match serde_json::from_str(value_json) {
            Ok(v) => v,
            Err(_) => return false,
        };
        self.state_doc.set_comm_state_property(comm_id, key, &value)
    }

    /// Set multiple properties in a comm's state map at once.
    ///
    /// Accepts a JSON object string of key-value pairs to write.
    /// Used by anywidget's `save_changes()` which batches pending mutations.
    /// Call `flush_runtime_state_sync()` after to propagate.
    pub fn set_comm_state_batch(&mut self, comm_id: &str, patch_json: &str) -> bool {
        let patch: serde_json::Value = match serde_json::from_str(patch_json) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let Some(obj) = patch.as_object() else {
            return false;
        };
        let mut any_written = false;
        for (key, value) in obj {
            if self.state_doc.set_comm_state_property(comm_id, key, value) {
                any_written = true;
            }
        }
        any_written
    }

    /// Generate a sync reply for the RuntimeStateDoc.
    /// Called immediately after each `RuntimeStateSyncApplied` event
    /// so the daemon knows which state the client has received.
    pub fn generate_runtime_state_sync_reply(&mut self) -> Option<Vec<u8>> {
        self.state_doc
            .generate_sync_message(&mut self.state_sync_state)
            .map(|msg| msg.encode())
    }

    /// Generate an initial RuntimeStateDoc sync message.
    ///
    /// Call this during bootstrap (alongside `flush_local_changes` for the
    /// notebook doc) so the daemon knows we need the full RuntimeStateDoc.
    /// Without this, if the daemon's initial `RuntimeStateSync` frame arrives
    /// before the WASM handle is ready, the kernel status is never synced
    /// and the frontend stays stuck on "not_started".
    ///
    /// If the returned message cannot be delivered, the caller MUST call
    /// `cancel_last_runtime_state_flush()` to prevent `sent_hashes` from
    /// permanently filtering out the undelivered state data.
    pub fn flush_runtime_state_sync(&mut self) -> Option<Vec<u8>> {
        self.state_doc
            .generate_sync_message(&mut self.state_sync_state)
            .map(|msg| msg.encode())
    }

    /// Roll back runtime-state sync state after a failed
    /// `flush_runtime_state_sync()` delivery.
    ///
    /// Mirrors `cancel_last_flush()` for the notebook doc: clears
    /// `in_flight` and `sent_hashes` on `state_sync_state` so the next
    /// `flush_runtime_state_sync()` or `generate_runtime_state_sync_reply()`
    /// produces a message instead of returning `None`.
    pub fn cancel_last_runtime_state_flush(&mut self) {
        self.state_sync_state.in_flight = false;
        self.state_sync_state.sent_hashes.clear();
    }

    /// Generate a sync reply for the PoolDoc.
    pub fn generate_pool_state_sync_reply(&mut self) -> Option<Vec<u8>> {
        self.pool_doc
            .generate_sync_message(&mut self.pool_sync_state)
            .map(|msg| msg.encode())
    }

    /// Generate an initial PoolDoc sync message.
    ///
    /// Call this during bootstrap so the daemon syncs pool state.
    pub fn flush_pool_state_sync(&mut self) -> Option<Vec<u8>> {
        self.pool_doc
            .generate_sync_message(&mut self.pool_sync_state)
            .map(|msg| msg.encode())
    }

    /// Roll back pool sync state after a failed delivery.
    pub fn cancel_last_pool_state_flush(&mut self) {
        self.pool_sync_state.in_flight = false;
        self.pool_sync_state.sent_hashes.clear();
    }

    /// Read the current pool state snapshot from the WASM doc.
    pub fn get_pool_state(&self) -> JsValue {
        let state = self.pool_doc.read_state();
        serialize_to_js(&state).unwrap_or(JsValue::UNDEFINED)
    }

    /// Read the current runtime state snapshot from the WASM doc.
    pub fn get_runtime_state(&self) -> JsValue {
        let state = self.state_doc.read_state();
        serialize_to_js(&state).unwrap_or(JsValue::UNDEFINED)
    }

    /// Resolve ContentRef values in a comm's state for frontend consumption.
    ///
    /// Walks the state **recursively**, resolving ContentRef objects:
    /// - `{"blob": hash, "size": N, "media_type": M?}` → plain URL string
    /// - `{"inline": value}` → unwrapped inner value
    /// - Plain values → passed through unchanged
    ///
    /// Returns `{ state, buffer_paths, text_paths }`:
    /// - `buffer_paths` — JSON paths of blob refs with binary MIME types (or no
    ///   media_type). The caller fetches these as ArrayBuffers for ipywidgets
    ///   buffer handling.
    /// - `text_paths` — JSON paths of blob refs whose `media_type` classifies
    ///   as text (`text/*`, `application/json`, `application/javascript`, etc.).
    ///   The caller must fetch each URL, decode as UTF-8, and replace the URL
    ///   string at that path with the decoded content before handing the state
    ///   to widget code. Widgets that consume synced string traits (e.g.
    ///   anywidget `_py_render`) expect the actual content, not a URL.
    ///
    /// Returns undefined if blob_port is not set or comm doesn't exist.
    pub fn resolve_comm_state(&self, comm_id: &str) -> JsValue {
        let Some(port) = self.blob_port else {
            return JsValue::UNDEFINED;
        };
        let state = self.state_doc.read_state();
        let Some(entry) = state.comms.get(comm_id) else {
            return JsValue::UNDEFINED;
        };

        let mut buffer_paths: Vec<Vec<String>> = Vec::new();
        let mut text_paths: Vec<Vec<String>> = Vec::new();
        let resolved = walk_and_resolve_comm_state(
            &entry.state,
            port,
            &mut Vec::new(),
            &mut buffer_paths,
            &mut text_paths,
        );

        serialize_to_js(&serde_json::json!({
            "state": resolved,
            "buffer_paths": buffer_paths,
            "text_paths": text_paths,
        }))
        .unwrap_or(JsValue::UNDEFINED)
    }

    /// Reset the sync state. Call this when reconnecting to a new daemon session.
    pub fn reset_sync_state(&mut self) {
        self.sync_state = sync::State::new();
        self.state_sync_state = sync::State::new();
        self.pool_sync_state = sync::State::new();
    }

    /// Normalize sync state by round-tripping through encode→decode.
    ///
    /// Preserves `shared_heads` (what we've agreed on with the daemon) while
    /// clearing all transient state (`sent_hashes`, `in_flight`, `their_heads`,
    /// `their_need`, etc.). This is the frontend equivalent of automerge-repo's
    /// `decodeSyncState(encodeSyncState(state))` defensive pattern, which
    /// prevents infinite sync loops caused by corrupted ephemeral state.
    ///
    /// Falls back to `State::new()` if encoding itself is corrupted.
    fn normalize_sync_state(&mut self) {
        let encoded = self.sync_state.encode();
        self.sync_state = sync::State::decode(&encoded).unwrap_or_else(|_| sync::State::new());
    }

    /// Normalize the RuntimeStateDoc sync state (same pattern as notebook sync).
    fn normalize_state_sync_state(&mut self) {
        let encoded = self.state_sync_state.encode();
        self.state_sync_state =
            sync::State::decode(&encoded).unwrap_or_else(|_| sync::State::new());
    }

    /// Rebuild the notebook doc via save→load to clear corrupted internal indices.
    ///
    /// Mirrors `NotebookDoc::rebuild_from_save()` on the daemon side. The
    /// `save()` path uses `op_set.export()` (safe even with corrupted indices),
    /// and `load()` reconstructs all internal data structures from scratch.
    fn rebuild_doc(&mut self) {
        let bytes = self.doc.save();
        if let Ok(doc) =
            NotebookDoc::load_with_encoding(&bytes, notebook_doc::TextEncoding::Utf16CodeUnit)
        {
            self.doc = doc;
        }
    }

    /// Rebuild the PoolDoc via save→load.
    fn rebuild_pool_doc(&mut self) {
        let bytes = self.pool_doc.doc_mut().save();
        if let Ok(doc) = automerge::AutoCommit::load(&bytes) {
            *self.pool_doc.doc_mut() = doc;
        }
    }

    /// Normalize pool sync state via encode→decode round-trip.
    fn normalize_pool_sync_state(&mut self) {
        let encoded = self.pool_sync_state.encode();
        self.pool_sync_state = sync::State::decode(&encoded).unwrap_or_else(|_| sync::State::new());
    }

    /// Rebuild the RuntimeStateDoc via save→load.
    fn rebuild_state_doc(&mut self) {
        let bytes = self.state_doc.doc_mut().save();
        if let Ok(doc) = automerge::AutoCommit::load(&bytes) {
            *self.state_doc.doc_mut() = doc;
        }
    }

    /// Receive a typed frame from the daemon, demux by type byte, return events for the frontend.
    ///
    /// The input is the raw frame bytes from the `notebook:frame` Tauri event:
    /// `[frame_type_byte, ...payload]`.
    ///
    /// Returns a JS array of `FrameEvent` objects directly via `serde-wasm-bindgen`
    /// (no JSON string intermediate). Sync frames return a single `sync_applied`
    /// event with an optional `CellChangeset` and an optional `reply`.
    ///
    /// **Sync replies are generated atomically** within this method after applying
    /// each inbound `AUTOMERGE_SYNC` frame. The reply bytes (if any) are returned
    /// in `FrameEvent::SyncApplied.reply` — the caller should send them immediately
    /// via `sendFrame(0x00, reply)`. This eliminates the consumption race from #1067
    /// where a separate `generate_sync_reply()` call could be preempted by
    /// `flushSync`'s `generate_sync_message()`, both competing on the same
    /// `sync_state`.
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
                    // Recovery: rebuild doc indices via save→load, then
                    // normalize sync state via encode→decode round-trip
                    // (preserves shared_heads, clears transient state).
                    // Finally generate a fresh sync message to restart
                    // negotiation with the daemon.
                    self.rebuild_doc();
                    self.normalize_sync_state();
                    // Check if the doc advanced before the error (partial apply).
                    // If so, the UI needs a full materialization to reflect the
                    // recovered state — otherwise it can stay stuck or stale.
                    let heads_after = self.doc.doc_mut().get_heads();
                    let changed = heads_before != heads_after;
                    if changed {
                        self.metadata_fingerprint_cache = None;
                    }
                    let reply = self
                        .doc
                        .generate_sync_message(&mut self.sync_state)
                        .map(|msg| msg.encode());
                    events.push(FrameEvent::SyncError { changed, reply });
                    return serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED);
                }
                let heads_after = self.doc.doc_mut().get_heads();
                let changed = heads_before != heads_after;

                let (changeset, attributions) = if changed {
                    // One doc.diff() walk covers cells + metadata + text patches.
                    let doc_changeset = diff_doc(self.doc.doc_mut(), &heads_before, &heads_after);
                    // Invalidate the fingerprint cache only when the notebook
                    // metadata actually moved — cell source edits and output
                    // streaming no longer bust the cache.
                    if doc_changeset.metadata_changed {
                        self.metadata_fingerprint_cache = None;
                    }
                    let attrs = build_text_attributions(
                        self.doc.doc_mut(),
                        &heads_before,
                        &doc_changeset.text_patches,
                    );
                    (Some(doc_changeset.cells), attrs)
                } else {
                    (None, Vec::new())
                };

                // Generate sync reply atomically — within the same &mut self
                // borrow as receive_sync_message. This eliminates the race from
                // #1067 where a separate generate_sync_reply() call could be
                // preempted by flushSync's generate_sync_message().
                let reply = self
                    .doc
                    .generate_sync_message(&mut self.sync_state)
                    .map(|msg| msg.encode());

                events.push(FrameEvent::SyncApplied {
                    changed,
                    changeset,
                    attributions,
                    reply,
                });
            }
            frame_types::BROADCAST => {
                // Parse JSON broadcast payload
                let value = match serde_json::from_slice::<serde_json::Value>(payload) {
                    Ok(v) => v,
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("[wasm] broadcast frame parse failed: {e}").into(),
                        );
                        return JsValue::UNDEFINED;
                    }
                };
                events.push(FrameEvent::Broadcast { payload: value });
            }
            frame_types::PRESENCE => {
                // Decode CBOR presence and convert to JSON value for the frontend
                let msg = match presence::decode_message(payload) {
                    Ok(m) => m,
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("[wasm] presence frame decode failed: {e}").into(),
                        );
                        return JsValue::UNDEFINED;
                    }
                };
                let value = match serde_json::to_value(&msg) {
                    Ok(v) => v,
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("[wasm] presence frame serialize failed: {e}").into(),
                        );
                        return JsValue::UNDEFINED;
                    }
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
                    // Recovery: rebuild state doc + normalize sync state,
                    // then generate a fresh sync message. Include the
                    // recovered state snapshot so the UI stays current.
                    self.rebuild_state_doc();
                    self.normalize_state_sync_state();
                    let heads_after = self.state_doc.doc_mut().get_heads();
                    let changed = heads_before != heads_after;
                    let state = if changed {
                        Some(Box::new(self.state_doc.read_state()))
                    } else {
                        None
                    };
                    let reply = self
                        .state_doc
                        .doc_mut()
                        .sync()
                        .generate_sync_message(&mut self.state_sync_state)
                        .map(|msg| msg.encode());
                    events.push(FrameEvent::RuntimeStateSyncError {
                        changed,
                        state,
                        reply,
                    });
                    return serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED);
                }
                let heads_after = self.state_doc.doc_mut().get_heads();
                let changed = heads_before != heads_after;

                let state = if changed {
                    Some(Box::new(self.state_doc.read_state()))
                } else {
                    None
                };

                // Diff outputs inline in executions — detect mid-execution
                // changes (stream append, display update, error). `state` is
                // Some iff `changed` is true, so this also covers the
                // `changed` branch without a separate check.
                let output_changed_cells = if let Some(current_state) = state.as_ref() {
                    let (changed_cells, new_snapshot) = diff_execution_outputs(
                        &self.prev_execution_outputs,
                        &current_state.executions,
                    );
                    self.prev_execution_outputs = new_snapshot;
                    changed_cells
                } else {
                    Vec::new()
                };

                events.push(FrameEvent::RuntimeStateSyncApplied {
                    changed,
                    state,
                    output_changed_cells,
                });
            }
            frame_types::POOL_STATE_SYNC => {
                // Apply daemon's PoolDoc sync message to our local replica.
                let Ok(msg) = sync::Message::decode(payload) else {
                    return JsValue::UNDEFINED;
                };
                let heads_before = self.pool_doc.doc_mut().get_heads();
                if self
                    .pool_doc
                    .doc_mut()
                    .sync()
                    .receive_sync_message(&mut self.pool_sync_state, msg)
                    .is_err()
                {
                    self.rebuild_pool_doc();
                    self.normalize_pool_sync_state();
                    let heads_after = self.pool_doc.doc_mut().get_heads();
                    let changed = heads_before != heads_after;
                    let state = if changed {
                        Some(Box::new(self.pool_doc.read_state()))
                    } else {
                        None
                    };
                    let reply = self
                        .pool_doc
                        .doc_mut()
                        .sync()
                        .generate_sync_message(&mut self.pool_sync_state)
                        .map(|msg| msg.encode());
                    events.push(FrameEvent::PoolStateSyncError {
                        changed,
                        state,
                        reply,
                    });
                    return serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED);
                }
                let heads_after = self.pool_doc.doc_mut().get_heads();
                let changed = heads_before != heads_after;

                let state = if changed {
                    Some(Box::new(self.pool_doc.read_state()))
                } else {
                    None
                };

                events.push(FrameEvent::PoolStateSyncApplied { changed, state });
            }
            _ => {
                events.push(FrameEvent::Unknown { frame_type });
            }
        }

        serialize_to_js(&events).unwrap_or(JsValue::UNDEFINED)
    }
}

// ── Attribution extraction ───────────────────────────────────────────

/// Combine raw text patches with actor attribution to produce
/// `TextAttribution` ranges for the frontend.
///
/// The text patches come from a single `diff_doc` walk, so this function
/// only does the actor-extraction query (`get_changes`) and zips the two
/// streams together. `diff()` no longer runs here — the caller already
/// paid for that walk.
fn build_text_attributions(
    doc: &mut automerge::AutoCommit,
    before: &[automerge::ChangeHash],
    text_patches: &[TextPatch],
) -> Vec<TextAttribution> {
    if text_patches.is_empty() {
        return Vec::new();
    }

    use std::collections::BTreeSet;
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

    text_patches
        .iter()
        .map(|tp| TextAttribution {
            cell_id: tp.cell_id.clone(),
            index: tp.index,
            text: tp.text.clone(),
            deleted: tp.deleted,
            actors: actors.clone(),
        })
        .collect()
}

// ── Presence encoding (free functions for wasm_bindgen export) ────────

/// Encode a cursor position as a presence frame payload (CBOR).
///
/// The frontend should prepend the frame type byte (0x04) and send
/// via `invoke("send_frame", { frameData })`.
///
/// `peer_label` is the human-readable name shown in cursor flags
/// (e.g. the OS username). Pass an empty string to omit.
#[wasm_bindgen]
pub fn encode_cursor_presence(
    peer_id: &str,
    peer_label: &str,
    actor_label: &str,
    cell_id: &str,
    line: u32,
    column: u32,
) -> Result<Vec<u8>, JsError> {
    let label = if peer_label.is_empty() {
        None
    } else {
        Some(peer_label)
    };
    let actor = if actor_label.is_empty() {
        None
    } else {
        Some(actor_label)
    };
    presence::encode_cursor_update_labeled(
        peer_id,
        label,
        actor,
        &presence::CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    )
    .map_err(|e| JsError::new(&e.to_string()))
}

/// Encode a selection range as a presence frame payload (CBOR).
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn encode_selection_presence(
    peer_id: &str,
    peer_label: &str,
    actor_label: &str,
    cell_id: &str,
    anchor_line: u32,
    anchor_col: u32,
    head_line: u32,
    head_col: u32,
) -> Result<Vec<u8>, JsError> {
    let label = if peer_label.is_empty() {
        None
    } else {
        Some(peer_label)
    };
    let actor = if actor_label.is_empty() {
        None
    } else {
        Some(actor_label)
    };
    presence::encode_selection_update_labeled(
        peer_id,
        label,
        actor,
        &presence::SelectionRange {
            cell_id: cell_id.to_string(),
            anchor_line,
            anchor_col,
            head_line,
            head_col,
        },
    )
    .map_err(|e| JsError::new(&e.to_string()))
}

/// Encode a cell focus as a presence frame payload (CBOR).
/// Focus means "I'm on this cell" without an editor cursor position.
#[wasm_bindgen]
pub fn encode_focus_presence(
    peer_id: &str,
    peer_label: &str,
    actor_label: &str,
    cell_id: &str,
) -> Result<Vec<u8>, JsError> {
    let label = if peer_label.is_empty() {
        None
    } else {
        Some(peer_label)
    };
    let actor = if actor_label.is_empty() {
        None
    } else {
        Some(actor_label)
    };
    presence::encode_focus_update_labeled(peer_id, label, actor, cell_id)
        .map_err(|e| JsError::new(&e.to_string()))
}

/// Encode a clear-channel message as a presence frame payload (CBOR).
/// Removes a single presence channel (e.g. cursor or selection) for this peer.
#[wasm_bindgen]
pub fn encode_clear_channel_presence(peer_id: &str, channel: &str) -> Result<Vec<u8>, JsError> {
    let ch = match channel {
        "cursor" => presence::Channel::Cursor,
        "selection" => presence::Channel::Selection,
        "focus" => presence::Channel::Focus,
        other => return Err(JsError::new(&format!("unknown presence channel: {other}"))),
    };
    presence::encode_clear_channel(peer_id, ch).map_err(|e| JsError::new(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resolve(val: serde_json::Value) -> (serde_json::Value, Vec<Vec<String>>, Vec<Vec<String>>) {
        let mut buffer_paths: Vec<Vec<String>> = Vec::new();
        let mut text_paths: Vec<Vec<String>> = Vec::new();
        let resolved = walk_and_resolve_comm_state(
            &val,
            1234,
            &mut Vec::new(),
            &mut buffer_paths,
            &mut text_paths,
        );
        (resolved, buffer_paths, text_paths)
    }

    #[test]
    fn inline_content_ref_is_unwrapped() {
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "value": { "inline": 42 },
        }));
        assert_eq!(resolved, json!({ "value": 42 }));
        assert!(buffer_paths.is_empty());
        assert!(text_paths.is_empty());
    }

    #[test]
    fn binary_blob_ref_goes_to_buffer_paths() {
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "image": { "blob": "abc123", "size": 100, "media_type": "image/png" },
        }));
        assert_eq!(
            resolved,
            json!({ "image": "http://127.0.0.1:1234/blob/abc123" })
        );
        assert_eq!(buffer_paths, vec![vec!["image".to_string()]]);
        assert!(text_paths.is_empty());
    }

    #[test]
    fn text_blob_ref_goes_to_text_paths() {
        // `_py_render` is consumed as a literal string by Pyodide, so it
        // must be text-inlined. `_esm` and `_css` are excluded from
        // text_paths even though their MIMEs are text — see the
        // `esm_and_css_stay_as_url` tests below for the rationale.
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "_py_render": { "blob": "def456", "size": 2048, "media_type": "text/plain" },
            "value": { "blob": "jkl012", "size": 4096, "media_type": "text/html" },
        }));
        assert_eq!(
            resolved["_py_render"],
            json!("http://127.0.0.1:1234/blob/def456")
        );
        assert_eq!(
            resolved["value"],
            json!("http://127.0.0.1:1234/blob/jkl012")
        );
        assert!(buffer_paths.is_empty());
        assert_eq!(text_paths.len(), 2);
        assert!(text_paths.contains(&vec!["_py_render".to_string()]));
        assert!(text_paths.contains(&vec!["value".to_string()]));
    }

    #[test]
    fn esm_blob_stays_as_url_not_text_inlined() {
        // Anywidget's `_esm` loads via native `import(url)`; pre-fetching
        // the text in the sync engine just to re-import it defeats
        // browser caching. Keep it on the URL path regardless of MIME.
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "_esm": { "blob": "esmhash", "size": 50000, "media_type": "text/javascript" },
        }));
        assert_eq!(
            resolved["_esm"],
            json!("http://127.0.0.1:1234/blob/esmhash")
        );
        assert_eq!(buffer_paths, vec![vec!["_esm".to_string()]]);
        assert!(
            text_paths.is_empty(),
            "_esm must not be inlined — loadESM handles URLs natively"
        );
    }

    #[test]
    fn css_blob_stays_as_url_not_text_inlined() {
        // Anywidget's `_css` is rendered as `<link rel=stylesheet
        // href=url>`; the browser fetches + caches it directly. No
        // reason to round-trip through the sync engine.
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "_css": { "blob": "csshash", "size": 2048, "media_type": "text/css" },
        }));
        assert_eq!(
            resolved["_css"],
            json!("http://127.0.0.1:1234/blob/csshash")
        );
        assert_eq!(buffer_paths, vec![vec!["_css".to_string()]]);
        assert!(
            text_paths.is_empty(),
            "_css must not be inlined — injectCSS handles URLs natively"
        );
    }

    #[test]
    fn esm_under_nested_parent_also_excluded() {
        // Exclusion matches the last path segment, not the full path.
        // Collateral: a stray `_esm` buried in user state is also
        // URL-routed. Acceptable since the identifier is reserved.
        let (_, buffer_paths, text_paths) = resolve(json!({
            "children": [
                {
                    "_esm": { "blob": "h", "size": 100, "media_type": "text/javascript" },
                },
            ],
        }));
        assert!(text_paths.is_empty());
        assert_eq!(
            buffer_paths,
            vec![vec![
                "children".to_string(),
                "0".to_string(),
                "_esm".to_string(),
            ]]
        );
    }

    #[test]
    fn svg_is_text_not_binary() {
        // image/svg+xml is a common gotcha — it's text despite the image/ prefix.
        let (_, buffer_paths, text_paths) = resolve(json!({
            "svg": { "blob": "s1", "size": 200, "media_type": "image/svg+xml" },
        }));
        assert!(buffer_paths.is_empty());
        assert_eq!(text_paths, vec![vec!["svg".to_string()]]);
    }

    #[test]
    fn missing_media_type_defaults_to_binary() {
        // Legacy comms without media_type must stay URL-shaped so existing
        // ipywidgets binary-buffer paths keep working.
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "buf": { "blob": "h", "size": 10 },
        }));
        assert_eq!(resolved, json!({ "buf": "http://127.0.0.1:1234/blob/h" }));
        assert_eq!(buffer_paths, vec![vec!["buf".to_string()]]);
        assert!(text_paths.is_empty());
    }

    #[test]
    fn application_json_is_text() {
        let (_, buffer_paths, text_paths) = resolve(json!({
            "spec": { "blob": "j", "size": 9999, "media_type": "application/json" },
        }));
        assert!(buffer_paths.is_empty());
        assert_eq!(text_paths, vec![vec!["spec".to_string()]]);
    }

    #[test]
    fn nested_paths_are_tracked_correctly() {
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "outer": {
                "list": [
                    { "inline": "first" },
                    { "blob": "binhash", "size": 1, "media_type": "image/png" },
                    { "blob": "txthash", "size": 1, "media_type": "text/plain" },
                ],
            },
        }));
        assert_eq!(resolved["outer"]["list"][0], json!("first"));
        assert_eq!(
            resolved["outer"]["list"][1],
            json!("http://127.0.0.1:1234/blob/binhash")
        );
        assert_eq!(
            resolved["outer"]["list"][2],
            json!("http://127.0.0.1:1234/blob/txthash")
        );
        assert_eq!(
            buffer_paths,
            vec![vec![
                "outer".to_string(),
                "list".to_string(),
                "1".to_string()
            ]]
        );
        assert_eq!(
            text_paths,
            vec![vec![
                "outer".to_string(),
                "list".to_string(),
                "2".to_string()
            ]]
        );
    }

    #[test]
    fn primitives_pass_through_unchanged() {
        let (resolved, buffer_paths, text_paths) = resolve(json!({
            "n": 1,
            "s": "hello",
            "b": true,
            "nil": null,
            "arr": [1, 2, 3],
        }));
        assert_eq!(
            resolved,
            json!({
                "n": 1,
                "s": "hello",
                "b": true,
                "nil": null,
                "arr": [1, 2, 3],
            })
        );
        assert!(buffer_paths.is_empty());
        assert!(text_paths.is_empty());
    }
}
