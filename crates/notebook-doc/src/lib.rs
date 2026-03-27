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
//! ## Document schema (v2)
//!
//! ```text
//! ROOT/
//!   schema_version: u64           ← Document schema version (2 = fractional-indexed map cells)
//!   notebook_id: Str
//!   cells/                        ← Map keyed by cell ID (O(1) lookup)
//!     {cell_id}/
//!       id: Str                   ← cell UUID (redundant but convenient)
//!       cell_type: Str            ← "code" | "markdown" | "raw"
//!       position: Str             ← Fractional index hex string for ordering
//!       source: Text              ← Automerge Text CRDT (character-level merging)
//!       execution_count: Str      ← JSON-encoded i32 or "null"
//!       outputs/                  ← List of Str
//!         [j]: Str                ← JSON-encoded Jupyter output (Phase 5: manifest hash)
//!       metadata/                 ← Map (native Automerge types, legacy: JSON string fallback)
//!       resolved_assets/          ← Map of markdown asset ref -> blob hash
//!   metadata/                     ← Map
//!     runtime: Str
//!     kernelspec/                 ← Map (native Automerge, per-field CRDT merge)
//!     language_info/              ← Map (native Automerge, per-field CRDT merge)
//!     runt/                       ← Map (native Automerge, per-field CRDT merge)
//!     notebook_metadata: Str      ← Legacy JSON string (backward compat, dual-written)
//! ```

pub mod diff;
pub mod frame_types;
pub mod metadata;
pub mod pep723;
pub mod presence;
pub mod runtime_state;

use std::collections::HashMap;

/// Current document schema version.
///
/// Bump this when making incompatible changes to the Automerge document
/// structure (e.g., switching cells from an ordered list to a fractional-indexed map).
///
/// - **1** — Original schema: `cells` is an ordered `List` of `Map`.
/// - **2** — Fractional indexing: `cells` is a `Map` keyed by cell ID, each cell has a `position` field.
pub const SCHEMA_VERSION: u64 = 2;

use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, AutomergeError, LoadOptions, ObjId, ObjType, ReadDoc};

/// Re-export so downstream crates (runtimed-wasm) can set text encoding
/// without depending on automerge directly.
pub use automerge::TextEncoding;
use loro_fractional_index::FractionalIndex;
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
    /// Fractional index hex string for ordering (e.g., "80", "7F80").
    /// Cells are sorted lexicographically by this field.
    pub position: String,
    pub source: String,
    /// JSON-encoded execution count: a number string like "5" or "null"
    pub execution_count: String,
    /// JSON-encoded Jupyter output objects (will become manifest hashes in Phase 5)
    pub outputs: Vec<String>,
    /// Cell metadata (arbitrary JSON object, preserves unknown keys)
    #[serde(default = "default_empty_object")]
    pub metadata: serde_json::Value,
    /// Resolved markdown asset refs (e.g. `attachment:image.png`, `images/foo.png`) → blob hash
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resolved_assets: HashMap<String, String>,
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

    /// Fork the document, creating an independent copy that shares history up
    /// to this point.
    ///
    /// Changes made on the fork are independent of the original. Call
    /// [`merge`](Self::merge) to reconcile them — Automerge's CRDT semantics
    /// handle concurrent edits (e.g., user typing while a formatter runs on
    /// the fork).
    pub fn fork(&mut self) -> Self {
        Self {
            doc: self.doc.fork(),
        }
    }

    /// Fork the document at a specific set of heads (historic point).
    ///
    /// The returned doc contains only the history up to `heads`. Changes
    /// made on the fork are treated as concurrent with any changes after
    /// `heads` in the original, enabling clean CRDT merges.
    pub fn fork_at(&mut self, heads: &[automerge::ChangeHash]) -> Result<Self, AutomergeError> {
        Ok(Self {
            doc: self.doc.fork_at(heads)?,
        })
    }

    /// Get the current document heads (change hashes at the tip).
    ///
    /// Store these after a save to enable `fork_at` for the file watcher —
    /// it can fork at the save point so external edits merge cleanly with
    /// post-save CRDT changes.
    pub fn get_heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// Merge another document's changes into this one.
    ///
    /// Returns the change hashes that were applied. Changes made on both
    /// sides since the fork point are merged using Automerge's CRDT rules —
    /// concurrent text edits at different positions compose cleanly.
    pub fn merge(
        &mut self,
        other: &mut NotebookDoc,
    ) -> Result<Vec<automerge::ChangeHash>, AutomergeError> {
        self.doc.merge(&mut other.doc)
    }

    /// Fork the document, apply mutations on the fork, and merge back.
    ///
    /// This is the preferred way to apply mutations that should compose
    /// with concurrent edits rather than overwriting them. The closure
    /// receives a forked doc; any mutations on it are merged back after
    /// the closure returns.
    ///
    /// ```ignore
    /// doc.fork_and_merge(|fork| {
    ///     fork.update_source("cell-1", "x = 1\n");
    ///     fork.delete_cell("cell-2");
    /// });
    /// ```
    ///
    /// For async work between fork and merge, use [`fork`](Self::fork)
    /// and [`merge`](Self::merge) directly — the fork must be created
    /// before the `.await` and merged after.
    pub fn fork_and_merge<F>(&mut self, f: F)
    where
        F: FnOnce(&mut NotebookDoc),
    {
        let mut fork = self.fork();
        f(&mut fork);
        let _ = self.merge(&mut fork);
    }

    /// Fork at a historic point, apply mutations, and merge back.
    ///
    /// Same as [`fork_and_merge`](Self::fork_and_merge) but forks at the
    /// given heads instead of the current state. Use this when applying
    /// external content (e.g., from disk) that corresponds to a known
    /// save point — the mutations are treated as concurrent with any
    /// changes after `heads`.
    ///
    /// Returns `Err` if the heads are unknown to this document.
    pub fn fork_at_and_merge<F>(
        &mut self,
        heads: &[automerge::ChangeHash],
        f: F,
    ) -> Result<(), AutomergeError>
    where
        F: FnOnce(&mut NotebookDoc),
    {
        let mut fork = self.fork_at(heads)?;
        f(&mut fork);
        let _ = self.merge(&mut fork);
        Ok(())
    }

    /// Set the actor identity for this document.
    ///
    /// Every Automerge operation is tagged with the actor ID of the document
    /// that created it. By default, `AutoCommit::new()` assigns a random UUID.
    /// Call this to set a meaningful, self-attested identity (e.g., `"runtimed"`,
    /// `"human"`, `"agent:claude"`) so edits are attributable to their source.
    ///
    /// The actor ID is encoded as the UTF-8 bytes of the label. Each peer
    /// session should use a unique actor ID — append a session suffix if
    /// multiple peers share the same label (e.g., `"human:<session-uuid>"`).
    pub fn set_actor(&mut self, actor_label: &str) {
        self.doc.set_actor(ActorId::from(actor_label.as_bytes()));
    }

    /// Get the actor identity label for this document.
    ///
    /// Returns the actor ID as a UTF-8 string if it's valid UTF-8,
    /// otherwise returns the hex representation.
    pub fn get_actor_id(&self) -> String {
        actor_label_from_id(self.doc.get_actor())
    }
}

/// Convert an Automerge [`ActorId`] to a human-readable label.
///
/// Actor labels in this project are UTF-8 strings encoded as `ActorId` bytes
/// (see [`NotebookDoc::set_actor`]).  This function reverses the encoding,
/// falling back to the hex representation for IDs that aren't valid UTF-8
/// (e.g., the random UUIDs assigned by `AutoCommit::new()`).
pub fn actor_label_from_id(actor: &ActorId) -> String {
    std::str::from_utf8(actor.to_bytes())
        .map(|s| s.to_string())
        .unwrap_or_else(|_| actor.to_hex_string())
}

// ── Native Automerge JSON storage ───────────────────────────────────

impl NotebookDoc {
    /// Recursively write a JSON value as native Automerge types at a map key.
    ///
    /// - `Value::Object` → `ObjType::Map`
    /// - `Value::Array`  → `ObjType::List`
    /// - `Value::Null`   → `ScalarValue::Null`
    /// - `Value::Bool`   → bool scalar
    /// - `Value::Number` → i64, u64, or f64 (tried in that order)
    /// - `Value::String` → string scalar
    pub fn put_json_value(
        &mut self,
        parent: &ObjId,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), AutomergeError> {
        put_json_at_key(&mut self.doc, parent, key, value)
    }

    /// Read an Automerge subtree back as a JSON value.
    ///
    /// Maps → `Value::Object`, Lists → `Value::Array`, Text → `Value::String`,
    /// scalars → corresponding JSON types.
    pub fn get_json_value(&self, parent: &ObjId, key: &str) -> Option<serde_json::Value> {
        read_json_value(&self.doc, parent, key)
    }

    /// Write a top-level metadata key as native Automerge types.
    pub fn set_metadata_value(
        &mut self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), AutomergeError> {
        let meta_id = match self.metadata_map_id() {
            Some(id) => id,
            None => self
                .doc
                .put_object(automerge::ROOT, "metadata", ObjType::Map)?,
        };
        put_json_at_key(&mut self.doc, &meta_id, key, value)
    }

    /// Read a top-level metadata key as JSON.
    pub fn get_metadata_value(&self, key: &str) -> Option<serde_json::Value> {
        let meta_id = self.metadata_map_id()?;
        read_json_value(&self.doc, &meta_id, key)
    }
}

// ── Typed metadata helpers ──────────────────────────────────────────

impl NotebookDoc {
    /// Read the notebook metadata as a typed snapshot.
    ///
    /// Reads native Automerge keys (`kernelspec`, `language_info`, `runt`).
    /// Returns `None` if no metadata keys are present.
    pub fn get_metadata_snapshot(&self) -> Option<metadata::NotebookMetadataSnapshot> {
        let meta_id = self.metadata_map_id()?;

        let kernelspec = read_json_value(&self.doc, &meta_id, "kernelspec")
            .and_then(|v| serde_json::from_value::<metadata::KernelspecSnapshot>(v).ok());
        let language_info = read_json_value(&self.doc, &meta_id, "language_info")
            .and_then(|v| serde_json::from_value::<metadata::LanguageInfoSnapshot>(v).ok());
        let runt = read_json_value(&self.doc, &meta_id, "runt")
            .and_then(|v| serde_json::from_value::<metadata::RuntMetadata>(v).ok());

        if kernelspec.is_some() || language_info.is_some() || runt.is_some() {
            return Some(metadata::NotebookMetadataSnapshot {
                kernelspec,
                language_info,
                runt: runt.unwrap_or_default(),
            });
        }

        None
    }

    /// Write a typed metadata snapshot to the document.
    ///
    /// Writes each top-level key (`kernelspec`, `language_info`, `runt`) as native
    /// Automerge maps for per-field CRDT merging.
    pub fn set_metadata_snapshot(
        &mut self,
        snapshot: &metadata::NotebookMetadataSnapshot,
    ) -> Result<(), AutomergeError> {
        let meta_id = match self.metadata_map_id() {
            Some(id) => id,
            None => self
                .doc
                .put_object(automerge::ROOT, "metadata", ObjType::Map)?,
        };

        match &snapshot.kernelspec {
            Some(ks) => {
                let v = serde_json::to_value(ks).map_err(|e| {
                    AutomergeError::InvalidObjId(format!("serialize kernelspec: {}", e))
                })?;
                put_json_at_key(&mut self.doc, &meta_id, "kernelspec", &v)?;
            }
            None => {
                let _ = self.doc.delete(&meta_id, "kernelspec");
            }
        }

        match &snapshot.language_info {
            Some(li) => {
                let v = serde_json::to_value(li).map_err(|e| {
                    AutomergeError::InvalidObjId(format!("serialize language_info: {}", e))
                })?;
                put_json_at_key(&mut self.doc, &meta_id, "language_info", &v)?;
            }
            None => {
                let _ = self.doc.delete(&meta_id, "language_info");
            }
        }

        let runt_v = serde_json::to_value(&snapshot.runt)
            .map_err(|e| AutomergeError::InvalidObjId(format!("serialize runt: {}", e)))?;
        put_json_at_key(&mut self.doc, &meta_id, "runt", &runt_v)?;

        Ok(())
    }

    /// Detect the notebook runtime from metadata (kernelspec + language_info).
    ///
    /// Returns `"python"`, `"deno"`, or `None` for unknown runtimes.
    /// Delegates to [`metadata::NotebookMetadataSnapshot::detect_runtime`].
    pub fn detect_runtime(&self) -> Option<String> {
        self.get_metadata_snapshot()?.detect_runtime()
    }

    /// Return a stable fingerprint of the notebook metadata.
    ///
    /// This is a cheap JSON serialization of the metadata snapshot, suitable
    /// for equality comparison. Consumers can compare fingerprints across sync
    /// batches to detect whether metadata actually changed — avoiding the cost
    /// of deserializing the full snapshot when it hasn't.
    ///
    /// Deterministic because `RuntMetadata.extra` uses `BTreeMap` (sorted keys).
    ///
    /// Returns `None` if no metadata is present.
    pub fn get_metadata_fingerprint(&self) -> Option<String> {
        let snapshot = self.get_metadata_snapshot()?;
        serde_json::to_string(&snapshot).ok()
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

    /// Set UV prerelease strategy, preserving deps and requires-python.
    /// Creates the metadata snapshot and UV section if absent.
    /// Pass "allow", "disallow", "if-necessary", "explicit", or "if-necessary-or-explicit".
    pub fn set_uv_prerelease(&mut self, prerelease: Option<String>) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_uv_prerelease(prerelease);
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
        Self::new_inner(notebook_id, None, None)
    }

    /// Create a new notebook document with a specific text encoding.
    ///
    /// Use `TextEncoding::Utf16CodeUnit` when positions come from a UTF-16
    /// environment (JavaScript / CodeMirror). The daemon should use the
    /// default (`UnicodeCodePoint`) since Python string indices are code points.
    /// Encoding is a local interpretation — it does not affect the wire format,
    /// so peers with different encodings sync correctly.
    pub fn new_with_encoding(notebook_id: &str, encoding: TextEncoding) -> Self {
        Self::new_inner(notebook_id, None, Some(encoding))
    }

    /// Create a new notebook document with a specific actor identity.
    ///
    /// Sets the actor ID **before** the initial structural operations so every
    /// operation in the document — including the schema, cells map, and metadata
    /// scaffolding — is attributed to `actor_label`.
    pub fn new_with_actor(notebook_id: &str, actor_label: &str) -> Self {
        Self::new_inner(notebook_id, Some(actor_label), None)
    }

    /// Shared constructor: optionally sets the actor before any mutations so
    /// that even the structural bootstrap operations are properly attributed.
    ///
    /// `AutoCommit::set_actor()` commits any pending transaction before
    /// changing the actor, so the actor must be set before the first `put`
    /// call — otherwise the initial change would be attributed to a random
    /// UUID instead of the intended label.
    fn new_inner(
        notebook_id: &str,
        actor_label: Option<&str>,
        encoding: Option<TextEncoding>,
    ) -> Self {
        let mut doc = match encoding {
            Some(enc) => AutoCommit::new_with_encoding(enc),
            None => AutoCommit::new(),
        };

        // Set actor *before* any puts so the initial structural change is
        // attributed to the caller, not a throwaway random UUID.
        if let Some(label) = actor_label {
            doc.set_actor(ActorId::from(label.as_bytes()));
        }

        let _ = doc.put(automerge::ROOT, "schema_version", SCHEMA_VERSION);
        let _ = doc.put(automerge::ROOT, "notebook_id", notebook_id);

        // cells: empty Map (keyed by cell ID, with fractional indexing for order)
        let _ = doc.put_object(automerge::ROOT, "cells", ObjType::Map);

        // metadata: Map with default runtime
        if let Ok(meta_id) = doc.put_object(automerge::ROOT, "metadata", ObjType::Map) {
            let _ = doc.put(&meta_id, "runtime", "python");
        }

        Self { doc }
    }

    /// Read the schema version from the document, if present.
    ///
    /// Returns `None` for documents created before schema versioning was added.
    /// Callers can treat `None` as schema version 1 (the original format).
    pub fn schema_version(&self) -> Option<u64> {
        match self.doc.get(automerge::ROOT, "schema_version").ok()?? {
            (automerge::Value::Scalar(s), _) => match s.as_ref() {
                automerge::ScalarValue::Uint(v) => Some(*v),
                automerge::ScalarValue::Int(v) => Some(*v as u64),
                _ => None,
            },
            _ => None,
        }
    }

    /// Migrate a v1 document (cells as List) to v2 (cells as Map with fractional positions).
    ///
    /// Reads all cells from the old List, deletes it, creates a Map, and
    /// re-inserts each cell with a fractional position. Cell content (source,
    /// outputs, execution_count, metadata) is preserved.
    pub fn migrate_v1_to_v2(&mut self) -> Result<(), AutomergeError> {
        use loro_fractional_index::FractionalIndex;

        // Idempotent: skip if already at current schema version
        if self.schema_version().unwrap_or(0) >= SCHEMA_VERSION {
            return Ok(());
        }

        // Only migrate if cells is actually a List (v1 schema).
        // If cells is already a Map or missing, create a fresh empty Map.
        let old_cells_id = self
            .doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::List) => Some(id),
                _ => None,
            });

        let old_cells = if let Some(ref cells_id) = old_cells_id {
            let len = self.doc.length(cells_id);
            let mut cells = Vec::with_capacity(len);
            for i in 0..len {
                if let Some(snap) = self.read_cell_from_obj(cells_id, i) {
                    cells.push(snap);
                }
            }
            cells
        } else {
            // No v1 List found — cells is either missing or already a Map.
            // Just bump schema version and return.
            self.doc
                .put(automerge::ROOT, "schema_version", SCHEMA_VERSION)?;
            return Ok(());
        };

        // Delete old List and create new Map
        let _ = self.doc.delete(automerge::ROOT, "cells");

        let cells_map = self
            .doc
            .put_object(automerge::ROOT, "cells", ObjType::Map)?;

        // Re-insert cells with fractional positions
        let mut prev_position: Option<FractionalIndex> = None;
        for cell in &old_cells {
            let position = match &prev_position {
                None => FractionalIndex::default(),
                Some(prev) => FractionalIndex::new_after(prev),
            };
            let position_str = position.to_string();

            let cell_obj = self
                .doc
                .put_object(&cells_map, cell.id.as_str(), ObjType::Map)?;
            self.doc.put(&cell_obj, "id", cell.id.as_str())?;
            self.doc
                .put(&cell_obj, "cell_type", cell.cell_type.as_str())?;
            self.doc.put(&cell_obj, "position", position_str.as_str())?;

            let source_id = self.doc.put_object(&cell_obj, "source", ObjType::Text)?;
            if !cell.source.is_empty() {
                self.doc.splice_text(&source_id, 0, 0, &cell.source)?;
            }

            self.doc
                .put(&cell_obj, "execution_count", cell.execution_count.as_str())?;

            let outputs_id = self.doc.put_object(&cell_obj, "outputs", ObjType::List)?;
            for (j, output) in cell.outputs.iter().enumerate() {
                self.doc.insert(&outputs_id, j, output.as_str())?;
            }

            if cell.metadata != serde_json::Value::Object(serde_json::Map::new()) {
                let meta_str =
                    serde_json::to_string(&cell.metadata).unwrap_or_else(|_| "{}".to_string());
                self.doc.put(&cell_obj, "metadata", meta_str.as_str())?;
            }

            prev_position = Some(position);
        }

        // Bump schema version
        self.doc
            .put(automerge::ROOT, "schema_version", SCHEMA_VERSION)?;

        #[cfg(feature = "persistence")]
        info!(
            "[notebook-doc] Migrated {} cells from List to Map schema",
            old_cells.len()
        );
        Ok(())
    }

    /// Read a cell snapshot from an object at a given index in a List container.
    /// Used by migration to read cells from the old v1 List schema.
    fn read_cell_from_obj(&self, cells_id: &ObjId, index: usize) -> Option<CellSnapshot> {
        let cell_obj = self
            .doc
            .get(cells_id, index)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })?;
        self.read_cell(&cell_obj)
    }

    /// Create a client-side bootstrap document for sync.
    ///
    /// Every client — WASM frontend, Python bindings, future Swift, etc. —
    /// must start from the same document skeleton before syncing with the
    /// daemon.  The skeleton mirrors what [`NotebookDoc::new()`] creates:
    ///
    /// ```text
    /// ROOT/
    ///   schema_version: 2
    ///   cells: {}          (empty Map)
    ///   metadata: {}       (empty Map)
    /// ```
    ///
    /// Both parameters are required:
    ///
    /// - `encoding` — `Utf16CodeUnit` for WASM/CodeMirror (JS strings are
    ///   UTF-16), `UnicodeCodePoint` for Python bindings. Encoding is a
    ///   local interpretation — peers with different encodings sync correctly.
    ///
    /// - `actor_label` — identity string for edit attribution (e.g.,
    ///   `"human:kyle:session42"`, `"agent:claude:abc123"`). Set **before**
    ///   the bootstrap puts so all ops are attributed to the caller.
    ///
    /// **Why this matters**: Automerge's `load_incremental` has a fast-path
    /// for empty documents (`is_empty() == true`) that replaces `*self` with
    /// a freshly-loaded doc using **default** `LoadOptions` — discarding any
    /// encoding or actor we set.  A non-empty doc takes the normal
    /// incremental-apply path which preserves all settings.
    ///
    /// Because the daemon creates the same structure, the CRDT merge
    /// converges to identical values with no conflicts.
    pub fn bootstrap(encoding: TextEncoding, actor_label: &str) -> Self {
        let mut doc = AutoCommit::new_with_encoding(encoding);
        doc.set_actor(ActorId::from(actor_label.as_bytes()));

        // Seed the standard notebook skeleton — schema_version, empty cells
        // map, and empty metadata map.  The daemon writes these too, so the
        // CRDT merge converges with no conflicts.
        //
        // NOTE: We intentionally do NOT set a default runtime here.  The
        // runtime is determined by the notebook file or user choice, not
        // hardcoded in the bootstrap skeleton.
        let _ = doc.put(automerge::ROOT, "schema_version", SCHEMA_VERSION);
        let _ = doc.put_object(automerge::ROOT, "cells", ObjType::Map);
        let _ = doc.put_object(automerge::ROOT, "metadata", ObjType::Map);

        Self { doc }
    }

    /// Load a notebook document from saved bytes.
    pub fn load(data: &[u8]) -> Result<Self, AutomergeError> {
        let doc = AutoCommit::load(data)?;
        Ok(Self { doc })
    }

    /// Load a notebook document from saved bytes with a specific text encoding.
    pub fn load_with_encoding(data: &[u8], encoding: TextEncoding) -> Result<Self, AutomergeError> {
        let doc = AutoCommit::load_with_options(data, LoadOptions::new().text_encoding(encoding))?;
        Ok(Self { doc })
    }

    /// Load a notebook document from saved bytes with a specific actor identity.
    ///
    /// The loaded document retains its full history (including the original
    /// actors), but any new operations will be tagged with `actor_label`.
    pub fn load_with_actor(data: &[u8], actor_label: &str) -> Result<Self, AutomergeError> {
        let mut s = Self::load(data)?;
        s.set_actor(actor_label);
        Ok(s)
    }

    /// Load from file or create a new document if the file doesn't exist.
    ///
    /// If the file exists but is corrupt (read or decode failure), the broken
    /// file is renamed to `{path}.corrupt` and a fresh document is created.
    /// This avoids silent data loss while still allowing the daemon to proceed.
    #[cfg(feature = "persistence")]
    pub fn load_or_create(path: &Path, notebook_id: &str) -> Self {
        Self::load_or_create_inner(path, notebook_id, None)
    }

    /// Load from file or create, with a specific actor identity for new operations.
    ///
    /// For loaded documents, `set_actor` is safe — there is no pending
    /// transaction, so the actor simply applies to future operations.
    /// For fresh documents (file missing or corrupt), `new_with_actor` sets
    /// the actor before any structural puts so the initial change is properly
    /// attributed — even on the corrupt-file recovery path.
    #[cfg(feature = "persistence")]
    pub fn load_or_create_with_actor(path: &Path, notebook_id: &str, actor_label: &str) -> Self {
        Self::load_or_create_inner(path, notebook_id, Some(actor_label))
    }

    /// Shared implementation for `load_or_create` and `load_or_create_with_actor`.
    ///
    /// When `actor_label` is `Some`, every code path that creates a fresh
    /// document uses `new_with_actor` so the structural bootstrap operations
    /// are properly attributed (not just the post-load `set_actor` call).
    #[cfg(feature = "persistence")]
    fn load_or_create_inner(path: &Path, notebook_id: &str, actor_label: Option<&str>) -> Self {
        if path.exists() {
            match std::fs::read(path) {
                Ok(data) => match AutoCommit::load(&data) {
                    Ok(doc) => {
                        let mut loaded = Self { doc };
                        let version = loaded.schema_version().unwrap_or(1);
                        if version < SCHEMA_VERSION {
                            info!(
                                "[notebook-doc] Migrating schema v{} → v{} at {:?} for {}",
                                version, SCHEMA_VERSION, path, notebook_id
                            );
                            if let Err(e) = loaded.migrate_v1_to_v2() {
                                warn!(
                                    "[notebook-doc] Migration failed for {}: {}. Creating fresh doc.",
                                    notebook_id, e
                                );
                                // Fall through to create a fresh doc below
                            } else {
                                info!("[notebook-doc] Migration complete for {}", notebook_id);
                                if let Some(label) = actor_label {
                                    loaded.set_actor(label);
                                }
                                return loaded;
                            }
                        } else {
                            info!("[notebook-doc] Loaded from {:?} for {}", path, notebook_id);
                            if let Some(label) = actor_label {
                                loaded.set_actor(label);
                            }
                            return loaded;
                        }
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
        match actor_label {
            Some(label) => Self::new_with_actor(notebook_id, label),
            None => Self::new(notebook_id),
        }
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

    /// Round-trip save→load to rebuild internal automerge indices.
    ///
    /// Used after catching an automerge panic (upstream MissingOps bug in
    /// `collector.rs`). `save()` serializes via `op_set.export()` (safe),
    /// and `load()` reconstructs all internal data structures from scratch.
    /// This is the document-level equivalent of automerge-repo's
    /// `decodeSyncState(encodeSyncState(state))` round-trip hack.
    pub fn rebuild_from_save(&mut self) -> bool {
        let actor = self.doc.get_actor().clone();
        let bytes = self.doc.save();
        match AutoCommit::load(&bytes) {
            Ok(mut doc) => {
                doc.set_actor(actor);
                self.doc = doc;
                true
            }
            Err(_) => false,
        }
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
        match self.cells_map_id() {
            Some(id) => self.doc.length(&id),
            None => 0,
        }
    }

    /// Get all cells as snapshots, sorted by position.
    pub fn get_cells(&self) -> Vec<CellSnapshot> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return vec![],
        };

        // Iterate over all keys in the map
        let mut cells: Vec<CellSnapshot> = self
            .doc
            .keys(&cells_id)
            .filter_map(|key| {
                let cell_obj = self.cell_obj_id(&cells_id, &key)?;
                self.read_cell(&cell_obj)
            })
            .collect();

        // Sort by position, tiebreak on cell ID for deterministic order across peers
        cells.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
        cells
    }

    /// Get a single cell by ID (O(1) lookup).
    pub fn get_cell(&self, cell_id: &str) -> Option<CellSnapshot> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        self.read_cell(&cell_obj)
    }

    /// Get ordered cell IDs (sorted by position, tiebreak on ID).
    ///
    /// O(n log n) for the sort, but avoids serializing cell contents.
    pub fn get_cell_ids(&self) -> Vec<String> {
        self.get_cell_positions()
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    /// Get the ID of the last cell in document order.
    /// Single-pass O(n) — tracks the max position without sorting.
    pub fn last_cell_id(&self) -> Option<String> {
        let cells_id = self.cells_map_id()?;
        let mut best: Option<(String, String)> = None;
        for key in self.doc.keys(&cells_id) {
            let cell_obj = match self.cell_obj_id(&cells_id, &key) {
                Some(id) => id,
                None => continue,
            };
            let position =
                read_str(&self.doc, &cell_obj, "position").unwrap_or_else(|| "80".to_string());
            let is_greater = match &best {
                Some((bp, bid)) => (&position, &key) > (bp, bid),
                None => true,
            };
            if is_greater {
                best = Some((position, key));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Get the ID of the first cell in document order.
    /// Single-pass O(n) — tracks the min position without sorting.
    pub fn first_cell_id(&self) -> Option<String> {
        let cells_id = self.cells_map_id()?;
        let mut best: Option<(String, String)> = None;
        for key in self.doc.keys(&cells_id) {
            let cell_obj = match self.cell_obj_id(&cells_id, &key) {
                Some(id) => id,
                None => continue,
            };
            let position =
                read_str(&self.doc, &cell_obj, "position").unwrap_or_else(|| "80".to_string());
            let is_less = match &best {
                Some((bp, bid)) => (&position, &key) < (bp, bid),
                None => true,
            };
            if is_less {
                best = Some((position, key));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Get a cell's source text (O(1) lookup).
    pub fn get_cell_source(&self, cell_id: &str) -> Option<String> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        let text_id = self.text_id(&cell_obj, "source")?;
        self.doc.text(&text_id).ok()
    }

    /// Get a cell's type — "code", "markdown", or "raw" (O(1) lookup).
    pub fn get_cell_type(&self, cell_id: &str) -> Option<String> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        read_str(&self.doc, &cell_obj, "cell_type")
    }

    /// Get a cell's outputs as a Vec of JSON strings (O(1) lookup).
    ///
    /// Each element is a JSON-encoded Jupyter output object (or manifest hash).
    pub fn get_cell_outputs(&self, cell_id: &str) -> Option<Vec<String>> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        let list_id = self.list_id(&cell_obj, "outputs")?;
        let len = self.doc.length(&list_id);
        Some(
            (0..len)
                .map(|i| read_str(&self.doc, &list_id, i).unwrap_or_default())
                .collect(),
        )
    }

    /// Get a cell's execution count (O(1) lookup).
    pub fn get_cell_execution_count(&self, cell_id: &str) -> Option<String> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        read_str(&self.doc, &cell_obj, "execution_count")
    }

    /// Get a cell's fractional index position string (O(1) lookup).
    pub fn get_cell_position(&self, cell_id: &str) -> Option<String> {
        let cell_obj = self.cell_obj_for(cell_id)?;
        read_str(&self.doc, &cell_obj, "position")
    }

    /// Insert a new cell at the given index (backward-compatible API).
    ///
    /// Internally converts the index to an `after_cell_id` and calls `add_cell_after`.
    /// Returns `Ok(())` on success. The cell starts with empty source, no outputs, and empty metadata.
    pub fn add_cell(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), AutomergeError> {
        // Convert index to after_cell_id. Indices greater than the current cell
        // count are treated as "insert at end" by clamping to ids.len().
        let ids = self.get_cell_ids(); // lightweight, position-sorted
        let clamped = index.min(ids.len());
        let after_cell_id = if clamped == 0 {
            None
        } else {
            ids.get(clamped - 1).map(|s| s.as_str())
        };

        self.add_cell_after(cell_id, cell_type, after_cell_id)?;
        Ok(())
    }

    /// Insert a new cell after the specified cell (semantic API).
    ///
    /// - `after_cell_id = None` → insert at the beginning
    /// - `after_cell_id = Some(id)` → insert after that cell
    ///
    /// Returns the position string of the new cell on success.
    pub fn add_cell_after(
        &mut self,
        cell_id: &str,
        cell_type: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, AutomergeError> {
        let cells_id = self
            .cells_map_id()
            .ok_or_else(|| AutomergeError::InvalidObjId("cells map not found".into()))?;

        let position = self.compute_position(after_cell_id);
        let position_str = position.to_string();

        // Create cell as a nested Map keyed by cell_id
        let cell_map = self.doc.put_object(&cells_id, cell_id, ObjType::Map)?;
        self.doc.put(&cell_map, "id", cell_id)?;
        self.doc.put(&cell_map, "cell_type", cell_type)?;
        self.doc.put(&cell_map, "position", position_str.as_str())?;
        self.doc.put_object(&cell_map, "source", ObjType::Text)?;
        self.doc.put(&cell_map, "execution_count", "null")?;
        self.doc.put_object(&cell_map, "outputs", ObjType::List)?;
        self.doc.put_object(&cell_map, "metadata", ObjType::Map)?;
        self.doc
            .put_object(&cell_map, "resolved_assets", ObjType::Map)?;

        Ok(position_str)
    }

    /// Insert a fully-populated cell with an explicit position string.
    ///
    /// This is the preferred method for bulk loads (e.g., loading from .ipynb).
    /// The caller provides the position string directly, avoiding O(n²) overhead
    /// from repeated `compute_position` calls.
    ///
    /// For bulk loads, generate positions incrementally:
    /// ```ignore
    /// let mut prev_position: Option<FractionalIndex> = None;
    /// for cell in ipynb_cells {
    ///     let position = match &prev_position {
    ///         None => FractionalIndex::default(),
    ///         Some(prev) => FractionalIndex::new_after(prev),
    ///     };
    ///     doc.add_cell_full(cell_id, cell_type, &position.to_string(), ...)?;
    ///     prev_position = Some(position);
    /// }
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn add_cell_full(
        &mut self,
        cell_id: &str,
        cell_type: &str,
        position: &str,
        source: &str,
        outputs: &[String],
        execution_count: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), AutomergeError> {
        let cells_id = self
            .cells_map_id()
            .ok_or_else(|| AutomergeError::InvalidObjId("cells map not found".into()))?;

        // Create cell as a nested Map keyed by cell_id
        let cell_map = self.doc.put_object(&cells_id, cell_id, ObjType::Map)?;
        self.doc.put(&cell_map, "id", cell_id)?;
        self.doc.put(&cell_map, "cell_type", cell_type)?;
        self.doc.put(&cell_map, "position", position)?;

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

        // Store metadata as native Automerge map
        let meta_map = self.doc.put_object(&cell_map, "metadata", ObjType::Map)?;
        if let Some(obj) = metadata.as_object() {
            for (k, v) in obj {
                put_json_at_key(&mut self.doc, &meta_map, k, v)?;
            }
        }

        self.doc
            .put_object(&cell_map, "resolved_assets", ObjType::Map)?;

        Ok(())
    }

    /// Delete a cell by ID (O(1) map delete). Returns `true` if the cell was found and deleted.
    pub fn delete_cell(&mut self, cell_id: &str) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };

        // Check if cell exists before deleting
        if self.cell_obj_id(&cells_id, cell_id).is_none() {
            return Ok(false);
        }

        self.doc.delete(&cells_id, cell_id)?;
        Ok(true)
    }

    /// Move a cell to a new position (after the specified cell).
    ///
    /// - `after_cell_id = None` → move to the beginning
    /// - `after_cell_id = Some(id)` → move after that cell
    ///
    /// This only updates the cell's `position` field — no delete/re-insert needed.
    /// Returns the new position string on success.
    ///
    /// ## Concurrent move semantics
    ///
    /// When two users move the same cell to different positions simultaneously,
    /// Automerge's last-write-wins (LWW) on the `position` scalar means one wins
    /// arbitrarily. After sync, both users see the same final position. This is
    /// acceptable behavior — it's a coordination problem between collaborators,
    /// not a data integrity issue.
    pub fn move_cell(
        &mut self,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, AutomergeError> {
        let cells_id = self
            .cells_map_id()
            .ok_or_else(|| AutomergeError::InvalidObjId("cells map not found".into()))?;

        let cell_obj = self
            .cell_obj_id(&cells_id, cell_id)
            .ok_or_else(|| AutomergeError::InvalidObjId(format!("cell not found: {}", cell_id)))?;

        let position = self.compute_position(after_cell_id);
        let position_str = position.to_string();

        self.doc.put(&cell_obj, "position", position_str.as_str())?;
        Ok(position_str)
    }

    /// Remove all cells from the document.
    ///
    /// Used to clean up after a failed streaming load so the next
    /// connection can retry from a clean state.
    pub fn clear_all_cells(&mut self) -> Result<(), AutomergeError> {
        if let Some(cells_id) = self.cells_map_id() {
            // Collect all cell IDs first to avoid modifying while iterating
            let cell_ids: Vec<String> = self.doc.keys(&cells_id).collect();
            for cell_id in cell_ids {
                self.doc.delete(&cells_id, &cell_id)?;
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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

    /// Splice a cell's source text at a specific position.
    ///
    /// Performs a character-level positional splice on the source Text CRDT:
    /// deletes `delete_count` characters starting at `index`, then inserts
    /// `text` at that position. This is the primitive that CodeMirror's
    /// `iterChanges` maps to directly — no Myers diff overhead.
    pub fn splice_source(
        &mut self,
        cell_id: &str,
        index: usize,
        delete_count: usize,
        text: &str,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };
        let source_id = match self.text_id(&cell_obj, "source") {
            Some(id) => id,
            None => return Ok(false),
        };

        let delete_count: isize = delete_count
            .try_into()
            .map_err(|_| AutomergeError::InvalidIndex(delete_count))?;
        self.doc
            .splice_text(&source_id, index, delete_count, text)?;
        Ok(true)
    }

    /// Append text to a cell's source without diffing.
    ///
    /// Unlike `update_source` which replaces the entire text (using Myers diff
    /// internally), this directly inserts characters at the end of the source
    /// Text CRDT. This is ideal for streaming/agentic use cases where an
    /// external process is appending tokens incrementally.
    pub fn append_source(&mut self, cell_id: &str, text: &str) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };
        let source_id = match self.text_id(&cell_obj, "source") {
            Some(id) => id,
            None => return Ok(false),
        };

        let len = self.doc.length(&source_id);
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok((false, 0)),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };

        // Iterate over all cell keys in the map
        let cell_ids: Vec<String> = self.doc.keys(&cells_id).collect();
        for cell_id in cell_ids {
            let cell_obj = match self.cell_obj_id(&cells_id, &cell_id) {
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

    /// Clear outputs and execution counts from every code cell.
    /// Returns the IDs of cells that were cleared.
    pub fn clear_all_outputs(&mut self) -> Result<Vec<String>, AutomergeError> {
        let cell_ids = self.get_cell_ids();
        let mut cleared = Vec::new();
        for cell_id in &cell_ids {
            if self.get_cell_type(cell_id).as_deref() == Some("code") {
                self.clear_outputs(cell_id)?;
                self.set_execution_count(cell_id, "null")?;
                cleared.push(cell_id.clone());
            }
        }
        Ok(cleared)
    }

    /// Get all outputs from all cells.
    ///
    /// Returns a list of (cell_id, output_index, output_string).
    /// Used by manifest-aware UpdateDisplayData handling.
    pub fn get_all_outputs(&self) -> Vec<(String, usize, String)> {
        let mut results = Vec::new();
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return results,
        };

        // Iterate over all cell keys in the map
        for cell_id in self.doc.keys(&cells_id) {
            let cell_obj = match self.cell_obj_id(&cells_id, &cell_id) {
                Some(o) => o,
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };

        self.doc.put(&cell_obj, "execution_count", count)?;
        Ok(true)
    }

    /// Set the execution_id pointer on a cell.
    ///
    /// The daemon stamps this at queue time so the frontend (and Python
    /// `Execution.result()`) can verify that cell outputs belong to the
    /// expected execution. Pass `None` to clear (e.g., on "clear outputs").
    pub fn set_execution_id(
        &mut self,
        cell_id: &str,
        execution_id: Option<&str>,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };

        match execution_id {
            Some(eid) => self.doc.put(&cell_obj, "execution_id", eid)?,
            None => self
                .doc
                .put(&cell_obj, "execution_id", automerge::ScalarValue::Null)?,
        }
        Ok(true)
    }

    /// Read the execution_id pointer from a cell, if set.
    pub fn get_execution_id(&self, cell_id: &str) -> Option<String> {
        let cells_id = self.cells_map_id()?;
        let cell_obj = self.cell_obj_id(&cells_id, cell_id)?;
        self.doc
            .get(&cell_obj, "execution_id")
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

    // ── Cell type ───────────────────────────────────────────────────

    /// Set the cell type for a cell. Valid values: "code", "markdown", "raw".
    pub fn set_cell_type(
        &mut self,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };

        self.doc.put(&cell_obj, "cell_type", cell_type)?;
        Ok(true)
    }

    // ── Cell metadata ──────────────────────────────────────────────

    /// Get the raw metadata Value for a cell.
    ///
    /// Reads native Automerge map first, falls back to legacy JSON string.
    /// Returns `None` if the cell doesn't exist.
    /// Returns `Some({})` if the cell exists but has no or invalid metadata.
    pub fn get_cell_metadata(&self, cell_id: &str) -> Option<serde_json::Value> {
        let cells_id = self.cells_map_id()?;
        let cell_obj = self.cell_obj_id(&cells_id, cell_id)?;
        Some(read_cell_metadata(&self.doc, &cell_obj))
    }

    /// Set the entire metadata object for a cell as a native Automerge map.
    ///
    /// Metadata is stored as native Automerge types (maps, lists, scalars) for
    /// per-field CRDT merging. Each call replaces the entire metadata map.
    /// Use `update_cell_metadata_at` for path-based updates when possible.
    pub fn set_cell_metadata(
        &mut self,
        cell_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };

        let meta_map = self.doc.put_object(&cell_obj, "metadata", ObjType::Map)?;
        if let Some(obj) = metadata.as_object() {
            for (k, v) in obj {
                put_json_at_key(&mut self.doc, &meta_map, k, v)?;
            }
        }
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

    // ── Resolved markdown assets ──────────────────────────────────────

    /// Get all resolved markdown asset refs for a cell (`ref` → blob hash).
    pub fn get_cell_resolved_assets(&self, cell_id: &str) -> Option<HashMap<String, String>> {
        let cells_id = self.cells_map_id()?;
        let cell_obj = self.cell_obj_id(&cells_id, cell_id)?;
        let resolved_assets_id = self.map_id(&cell_obj, "resolved_assets")?;

        Some(
            self.doc
                .map_range(&resolved_assets_id, ..)
                .filter_map(|item| {
                    if let automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(hash)) =
                        item.value
                    {
                        return Some((item.key.to_string(), hash.to_string()));
                    }
                    None
                })
                .collect(),
        )
    }

    /// Replace the resolved markdown asset refs for a cell.
    ///
    /// Returns `true` if the map changed.
    pub fn set_cell_resolved_assets(
        &mut self,
        cell_id: &str,
        resolved_assets: &HashMap<String, String>,
    ) -> Result<bool, AutomergeError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Ok(false),
        };

        let existing = self.get_cell_resolved_assets(cell_id).unwrap_or_default();
        if existing == *resolved_assets {
            return Ok(false);
        }

        let resolved_assets_id = match self.map_id(&cell_obj, "resolved_assets") {
            Some(id) => id,
            None => self
                .doc
                .put_object(&cell_obj, "resolved_assets", ObjType::Map)?,
        };

        for key in existing.keys() {
            if !resolved_assets.contains_key(key) {
                self.doc.delete(&resolved_assets_id, key)?;
            }
        }

        for (asset_ref, blob_hash) in resolved_assets {
            if existing.get(asset_ref) != Some(blob_hash) {
                self.doc.put(&resolved_assets_id, asset_ref, blob_hash)?;
            }
        }

        Ok(true)
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

    // ── Provenance queries ──────────────────────────────────────────

    /// Return the deduplicated, sorted list of actor labels that have
    /// contributed changes to this document.
    ///
    /// Walks the Automerge change history and converts each change's
    /// `ActorId` to a label via [`actor_label_from_id`].
    ///
    /// **Cost:** O(changes) — every change in the document history is
    /// visited on each call. Avoid calling in hot paths; cache the result
    /// when the document is known to be unchanged.
    ///
    /// This is useful for debugging ("who has touched this notebook?")
    /// and will underpin richer attribution queries in the future.
    pub fn contributing_actors(&mut self) -> Vec<String> {
        let changes = self.doc.get_changes(&[]);
        let mut seen = std::collections::BTreeSet::new();
        for change in &changes {
            seen.insert(actor_label_from_id(change.actor_id()));
        }
        seen.into_iter().collect()
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Get the cells Map object ID.
    fn cells_map_id(&self) -> Option<ObjId> {
        self.doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
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

    /// Convenience: look up a cell's ObjId by cell ID (two-step: cells map → cell map).
    fn cell_obj_for(&self, cell_id: &str) -> Option<ObjId> {
        let cells_id = self.cells_map_id()?;
        self.cell_obj_id(&cells_id, cell_id)
    }

    /// Get a cell's ObjId by its ID (O(1) map lookup).
    fn cell_obj_id(&self, cells_id: &ObjId, cell_id: &str) -> Option<ObjId> {
        self.doc
            .get(cells_id, cell_id)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    /// Get (position, cell_id) pairs sorted by position.
    /// Lightweight — only reads position strings, skips source/outputs/metadata.
    fn get_cell_positions(&self) -> Vec<(String, String)> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return vec![],
        };

        let mut pairs: Vec<(String, String)> = self
            .doc
            .keys(&cells_id)
            .filter_map(|key| {
                let cell_obj = self.cell_obj_id(&cells_id, &key)?;
                let position =
                    read_str(&self.doc, &cell_obj, "position").unwrap_or_else(|| "80".to_string());
                Some((position, key))
            })
            .collect();

        pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        pairs
    }

    /// Compute a position for a new cell.
    ///
    /// - `after_cell_id = None` → insert at start (before first cell)
    /// - `after_cell_id = Some(id)` → insert after that cell
    fn compute_position(&self, after_cell_id: Option<&str>) -> FractionalIndex {
        let pairs = self.get_cell_positions(); // (position, cell_id) sorted

        match after_cell_id {
            None => {
                // Insert at start
                pairs
                    .first()
                    .map(|(pos, _)| {
                        FractionalIndex::new_before(&FractionalIndex::from_hex_string(pos))
                    })
                    .unwrap_or_default()
            }
            Some(after_id) => {
                let idx = pairs.iter().position(|(_, id)| id == after_id);
                match idx {
                    Some(i) if i + 1 < pairs.len() => {
                        // Insert between after and next
                        FractionalIndex::new_between(
                            &FractionalIndex::from_hex_string(&pairs[i].0),
                            &FractionalIndex::from_hex_string(&pairs[i + 1].0),
                        )
                        .unwrap_or_else(|| {
                            // Fallback: insert after if between fails
                            FractionalIndex::new_after(&FractionalIndex::from_hex_string(
                                &pairs[i].0,
                            ))
                        })
                    }
                    Some(i) => {
                        // Insert at end (after the last cell)
                        FractionalIndex::new_after(&FractionalIndex::from_hex_string(&pairs[i].0))
                    }
                    None => {
                        // after_cell_id not found: insert at end (after the last cell)
                        pairs
                            .last()
                            .map(|(pos, _)| {
                                FractionalIndex::new_after(&FractionalIndex::from_hex_string(pos))
                            })
                            .unwrap_or_default()
                    }
                }
            }
        }
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

    fn map_id(&self, parent: &ObjId, key: &str) -> Option<ObjId> {
        self.doc
            .get(parent, key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn read_cell(&self, cell_obj: &ObjId) -> Option<CellSnapshot> {
        let id = read_str(&self.doc, cell_obj, "id")?;
        let cell_type = read_str(&self.doc, cell_obj, "cell_type").unwrap_or_default();
        let position =
            read_str(&self.doc, cell_obj, "position").unwrap_or_else(|| "80".to_string());
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
                    .map(|i| read_str(&self.doc, &list_id, i).unwrap_or_default())
                    .collect()
            }
            None => vec![],
        };

        // Read metadata (native Automerge map with legacy string fallback)
        let metadata = read_cell_metadata(&self.doc, cell_obj);

        // Read resolved asset map
        let resolved_assets = match self.map_id(cell_obj, "resolved_assets") {
            Some(map_id) => self
                .doc
                .map_range(&map_id, ..)
                .filter_map(|item| {
                    if let automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(hash)) =
                        item.value
                    {
                        return Some((item.key.to_string(), hash.to_string()));
                    }
                    None
                })
                .collect(),
            None => HashMap::new(),
        };

        Some(CellSnapshot {
            id,
            cell_type,
            position,
            source,
            execution_count,
            outputs,
            metadata,
            resolved_assets,
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

/// Convert an `automerge::ScalarValue` to a `serde_json::Value`.
fn scalar_to_json(s: &automerge::ScalarValue) -> Option<serde_json::Value> {
    match s {
        automerge::ScalarValue::Null => Some(serde_json::Value::Null),
        automerge::ScalarValue::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        automerge::ScalarValue::Int(i) => {
            Some(serde_json::Value::Number(serde_json::Number::from(*i)))
        }
        automerge::ScalarValue::Uint(u) => {
            Some(serde_json::Value::Number(serde_json::Number::from(*u)))
        }
        automerge::ScalarValue::F64(f) => Some(
            serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        ),
        automerge::ScalarValue::Str(s) => Some(serde_json::Value::String(s.to_string())),
        _ => None, // Timestamp, Counter, Bytes — not used for JSON metadata
    }
}

/// Recursively read an Automerge value (scalar, Map, List, or Text) as JSON.
fn read_json_value<P: Into<automerge::Prop>>(
    doc: &AutoCommit,
    parent: &ObjId,
    prop: P,
) -> Option<serde_json::Value> {
    let (value, obj_id) = doc.get(parent, prop).ok().flatten()?;
    match value {
        automerge::Value::Scalar(s) => scalar_to_json(s.as_ref()),
        automerge::Value::Object(ObjType::Map) => {
            let mut map = serde_json::Map::new();
            for key in doc.keys(&obj_id) {
                if let Some(v) = read_json_value(doc, &obj_id, key.as_str()) {
                    map.insert(key, v);
                }
            }
            Some(serde_json::Value::Object(map))
        }
        automerge::Value::Object(ObjType::List) => {
            let len = doc.length(&obj_id);
            let arr: Vec<serde_json::Value> = (0..len)
                .map(|i| read_json_value(doc, &obj_id, i).unwrap_or(serde_json::Value::Null))
                .collect();
            Some(serde_json::Value::Array(arr))
        }
        automerge::Value::Object(ObjType::Text) => {
            doc.text(&obj_id).ok().map(serde_json::Value::String)
        }
        _ => None,
    }
}

/// Recursively write a JSON value into an Automerge Map at a string key.
fn put_json_at_key(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.put(parent, key, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.put(parent, key, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.put(parent, key, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.put(parent, key, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.put(parent, key, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.put(parent, key, s.as_str())?;
        }
        serde_json::Value::Array(arr) => {
            let list_id = doc.put_object(parent, key, ObjType::List)?;
            for (i, item) in arr.iter().enumerate() {
                insert_json_at_index(doc, &list_id, i, item)?;
            }
        }
        serde_json::Value::Object(map) => {
            let map_id = doc.put_object(parent, key, ObjType::Map)?;
            for (k, v) in map {
                put_json_at_key(doc, &map_id, k, v)?;
            }
        }
    }
    Ok(())
}

/// Recursively insert a JSON value into an Automerge List at a given index.
fn insert_json_at_index(
    doc: &mut AutoCommit,
    parent: &ObjId,
    index: usize,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.insert(parent, index, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.insert(parent, index, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.insert(parent, index, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.insert(parent, index, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.insert(parent, index, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.insert(parent, index, s.as_str())?;
        }
        serde_json::Value::Array(arr) => {
            let list_id = doc.insert_object(parent, index, ObjType::List)?;
            for (i, item) in arr.iter().enumerate() {
                insert_json_at_index(doc, &list_id, i, item)?;
            }
        }
        serde_json::Value::Object(map) => {
            let map_id = doc.insert_object(parent, index, ObjType::Map)?;
            for (k, v) in map {
                put_json_at_key(doc, &map_id, k, v)?;
            }
        }
    }
    Ok(())
}

/// Read cell metadata with native Automerge map support and legacy string fallback.
///
/// Tries to read `metadata` as an `ObjType::Map` first (native storage),
/// falls back to reading as a JSON-encoded string (legacy storage).
fn read_cell_metadata(doc: &AutoCommit, cell_obj: &ObjId) -> serde_json::Value {
    match doc.get(cell_obj, "metadata").ok().flatten() {
        Some((automerge::Value::Object(ObjType::Map), map_id)) => {
            let mut obj = serde_json::Map::new();
            for key in doc.keys(&map_id) {
                if let Some(v) = read_json_value(doc, &map_id, key.as_str()) {
                    obj.insert(key, v);
                }
            }
            serde_json::Value::Object(obj)
        }
        Some((automerge::Value::Scalar(s), _)) => {
            if let automerge::ScalarValue::Str(s) = s.as_ref() {
                serde_json::from_str(s).unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            }
        }
        _ => serde_json::json!({}),
    }
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
///
/// Reads native Automerge keys (`kernelspec`, `language_info`, `runt`).
/// Returns `None` if no metadata keys are present.
pub fn get_metadata_snapshot_from_doc(
    doc: &AutoCommit,
) -> Option<metadata::NotebookMetadataSnapshot> {
    let meta_id = doc
        .get(automerge::ROOT, "metadata")
        .ok()
        .flatten()
        .and_then(|(value, id)| match value {
            automerge::Value::Object(ObjType::Map) => Some(id),
            _ => None,
        })?;

    let kernelspec = read_json_value(doc, &meta_id, "kernelspec")
        .and_then(|v| serde_json::from_value::<metadata::KernelspecSnapshot>(v).ok());
    let language_info = read_json_value(doc, &meta_id, "language_info")
        .and_then(|v| serde_json::from_value::<metadata::LanguageInfoSnapshot>(v).ok());
    let runt = read_json_value(doc, &meta_id, "runt")
        .and_then(|v| serde_json::from_value::<metadata::RuntMetadata>(v).ok());

    if kernelspec.is_some() || language_info.is_some() || runt.is_some() {
        return Some(metadata::NotebookMetadataSnapshot {
            kernelspec,
            language_info,
            runt: runt.unwrap_or_default(),
        });
    }

    None
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
///
/// Returns cells sorted by position.
pub fn get_cells_from_doc(doc: &AutoCommit) -> Vec<CellSnapshot> {
    let cells_id = match doc.get(automerge::ROOT, "cells").ok().flatten() {
        Some((automerge::Value::Object(ObjType::Map), id)) => id,
        _ => return vec![],
    };

    let mut cells: Vec<CellSnapshot> = doc
        .keys(&cells_id)
        .filter_map(|cell_id| {
            let cell_obj = match doc.get(&cells_id, &cell_id).ok().flatten() {
                Some((automerge::Value::Object(ObjType::Map), id)) => id,
                _ => return None,
            };

            let id = read_str(doc, &cell_obj, "id")?;
            let cell_type = read_str(doc, &cell_obj, "cell_type").unwrap_or_default();
            let position = read_str(doc, &cell_obj, "position").unwrap_or_else(|| "80".to_string());
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
                        .map(|j| read_str(doc, &list_id, j).unwrap_or_default())
                        .collect()
                }
                _ => vec![],
            };

            // Read metadata (native Automerge map with legacy string fallback)
            let metadata = read_cell_metadata(doc, &cell_obj);

            // Read resolved asset map
            let resolved_assets = match doc.get(&cell_obj, "resolved_assets").ok().flatten() {
                Some((automerge::Value::Object(ObjType::Map), map_id)) => doc
                    .map_range(&map_id, ..)
                    .filter_map(|item| {
                        if let automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(hash)) =
                            item.value
                        {
                            return Some((item.key.to_string(), hash.to_string()));
                        }
                        None
                    })
                    .collect(),
                _ => HashMap::new(),
            };

            Some(CellSnapshot {
                id,
                cell_type,
                position,
                source,
                execution_count,
                outputs,
                metadata,
                resolved_assets,
            })
        })
        .collect();

    // Sort by position, tiebreak on cell ID for deterministic order across peers
    cells.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    cells
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_doc_has_bootstrap_skeleton() {
        // empty() now delegates to bootstrap(), which seeds the doc with
        // schema_version, an empty cells map, and an empty metadata map.
        let doc = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        assert_eq!(doc.notebook_id(), None); // bootstrap doesn't set notebook_id
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(doc.get_cells(), vec![]);
        assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));
        // Bootstrap does NOT set a default runtime — that's determined by
        // the notebook file or user choice.
        assert_eq!(doc.get_metadata("runtime"), None);
    }

    #[test]
    fn test_empty_doc_set_metadata() {
        let mut doc = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
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

        let mut empty = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
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
            "cell-full",
            "code",
            "80", // position
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
        assert_eq!(cell.position, "80");
        assert_eq!(cell.source, "print('hello')");
        assert_eq!(cell.execution_count, "42");
        assert_eq!(cell.outputs, vec!["hash1", "hash2"]);
        assert_eq!(cell.tags(), vec!["test"]);
    }

    #[test]
    fn test_add_cell_full_empty_source() {
        let mut doc = NotebookDoc::new("nb-empty-src");
        doc.add_cell_full(
            "cell-es",
            "code",
            "80", // position
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
    fn test_add_cell_full_position_ordering() {
        use loro_fractional_index::FractionalIndex;

        let mut doc = NotebookDoc::new("nb-order");

        // Generate positions incrementally (like bulk load)
        let pos_a = FractionalIndex::default();
        let pos_b = FractionalIndex::new_after(&pos_a);
        let pos_c = FractionalIndex::new_after(&pos_b);

        doc.add_cell_full(
            "a",
            "code",
            &pos_a.to_string(),
            "first",
            &[],
            "null",
            &serde_json::json!({}),
        )
        .unwrap();
        doc.add_cell_full(
            "b",
            "code",
            &pos_b.to_string(),
            "second",
            &[],
            "null",
            &serde_json::json!({}),
        )
        .unwrap();
        doc.add_cell_full(
            "c",
            "code",
            &pos_c.to_string(),
            "third",
            &[],
            "null",
            &serde_json::json!({}),
        )
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
            "cell1",
            "code",
            "80", // position
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

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
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

    // ── Fractional indexing tests ─────────────────────────────────────

    #[test]
    fn test_add_cell_after_at_start() {
        let mut doc = NotebookDoc::new("nb-fi");
        doc.add_cell(0, "b", "code").unwrap();
        doc.add_cell(1, "c", "code").unwrap();

        // Add cell at start (before first)
        let pos = doc.add_cell_after("a", "code", None).unwrap();
        assert!(!pos.is_empty());

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "a");
        assert_eq!(cells[1].id, "b");
        assert_eq!(cells[2].id, "c");
    }

    #[test]
    fn test_add_cell_after_in_middle() {
        let mut doc = NotebookDoc::new("nb-fi");
        doc.add_cell(0, "a", "code").unwrap();
        doc.add_cell(1, "c", "code").unwrap();

        // Add cell after "a" (between a and c)
        doc.add_cell_after("b", "code", Some("a")).unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "a");
        assert_eq!(cells[1].id, "b");
        assert_eq!(cells[2].id, "c");
    }

    #[test]
    fn test_add_cell_after_at_end() {
        let mut doc = NotebookDoc::new("nb-fi");
        doc.add_cell(0, "a", "code").unwrap();
        doc.add_cell(1, "b", "code").unwrap();

        // Add cell after last
        doc.add_cell_after("c", "code", Some("b")).unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "a");
        assert_eq!(cells[1].id, "b");
        assert_eq!(cells[2].id, "c");
    }

    #[test]
    fn test_move_cell_to_start() {
        let mut doc = NotebookDoc::new("nb-move");
        doc.add_cell(0, "a", "code").unwrap();
        doc.add_cell(1, "b", "code").unwrap();
        doc.add_cell(2, "c", "code").unwrap();

        // Move c to start
        doc.move_cell("c", None).unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "c");
        assert_eq!(cells[1].id, "a");
        assert_eq!(cells[2].id, "b");
    }

    #[test]
    fn test_move_cell_to_middle() {
        let mut doc = NotebookDoc::new("nb-move");
        doc.add_cell(0, "a", "code").unwrap();
        doc.add_cell(1, "b", "code").unwrap();
        doc.add_cell(2, "c", "code").unwrap();

        // Move c after a (between a and b)
        doc.move_cell("c", Some("a")).unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "a");
        assert_eq!(cells[1].id, "c");
        assert_eq!(cells[2].id, "b");
    }

    #[test]
    fn test_move_cell_to_end() {
        let mut doc = NotebookDoc::new("nb-move");
        doc.add_cell(0, "a", "code").unwrap();
        doc.add_cell(1, "b", "code").unwrap();
        doc.add_cell(2, "c", "code").unwrap();

        // Move a to end (after c)
        doc.move_cell("a", Some("c")).unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].id, "b");
        assert_eq!(cells[1].id, "c");
        assert_eq!(cells[2].id, "a");
    }

    #[test]
    fn test_move_cell_preserves_content() {
        let mut doc = NotebookDoc::new("nb-move");
        doc.add_cell(0, "a", "code").unwrap();
        doc.update_source("a", "original source").unwrap();
        doc.set_outputs("a", &["output1".to_string()]).unwrap();
        doc.set_execution_count("a", "42").unwrap();

        doc.add_cell(1, "b", "code").unwrap();

        // Move a after b
        doc.move_cell("a", Some("b")).unwrap();

        // Verify content preserved
        let cell = doc.get_cell("a").unwrap();
        assert_eq!(cell.source, "original source");
        assert_eq!(cell.outputs, vec!["output1"]);
        assert_eq!(cell.execution_count, "42");
    }

    #[test]
    fn test_move_cell_nonexistent() {
        let mut doc = NotebookDoc::new("nb-move");
        doc.add_cell(0, "a", "code").unwrap();

        let result = doc.move_cell("nonexistent", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_position_ordering_stress() {
        // Insert many cells between two positions to stress test position generation
        let mut doc = NotebookDoc::new("nb-stress");
        doc.add_cell(0, "first", "code").unwrap();
        doc.add_cell(1, "last", "code").unwrap();

        // Insert 50 cells between first and last
        for i in 0..50 {
            let cell_id = format!("middle-{}", i);
            doc.add_cell_after(&cell_id, "code", Some("first")).unwrap();
        }

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 52);
        assert_eq!(cells[0].id, "first");
        assert_eq!(cells[51].id, "last");

        // Verify all positions are unique and properly ordered
        let mut prev_pos = String::new();
        for cell in &cells {
            assert!(
                cell.position > prev_pos,
                "Position {} should be > {}",
                cell.position,
                prev_pos
            );
            prev_pos = cell.position.clone();
        }
    }

    #[test]
    fn test_move_cell_sync() {
        use automerge::sync;

        let mut daemon = NotebookDoc::new("nb-sync");
        daemon.add_cell(0, "a", "code").unwrap();
        daemon.add_cell(1, "b", "code").unwrap();
        daemon.add_cell(2, "c", "code").unwrap();

        // Sync to client
        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        for _ in 0..3 {
            if let Some(msg) = daemon.generate_sync_message(&mut daemon_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                daemon.receive_sync_message(&mut daemon_state, msg).unwrap();
            }
        }

        // Move cell on daemon
        daemon.move_cell("c", None).unwrap();

        // Sync again
        for _ in 0..3 {
            if let Some(msg) = daemon.generate_sync_message(&mut daemon_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                daemon.receive_sync_message(&mut daemon_state, msg).unwrap();
            }
        }

        // Verify client sees the new order
        let cells = client.get_cells();
        assert_eq!(cells[0].id, "c");
        assert_eq!(cells[1].id, "a");
        assert_eq!(cells[2].id, "b");
    }

    /// Build a v1 (List-based) document for migration testing.
    fn make_v1_doc(notebook_id: &str) -> NotebookDoc {
        let mut doc = AutoCommit::new();
        let _ = doc.put(automerge::ROOT, "schema_version", 1u64);
        let _ = doc.put(automerge::ROOT, "notebook_id", notebook_id);
        let _ = doc.put_object(automerge::ROOT, "cells", ObjType::List);
        if let Ok(meta_id) = doc.put_object(automerge::ROOT, "metadata", ObjType::Map) {
            let _ = doc.put(&meta_id, "runtime", "python");
        }
        NotebookDoc { doc }
    }

    /// Add a cell to a v1 doc using List insert (the old schema).
    fn v1_add_cell(
        doc: &mut NotebookDoc,
        index: usize,
        cell_id: &str,
        cell_type: &str,
        source: &str,
    ) {
        let cells_id = doc
            .doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .map(|(_, id)| id)
            .unwrap();
        let len = doc.doc.length(&cells_id);
        let index = index.min(len);
        let cell_map = doc
            .doc
            .insert_object(&cells_id, index, ObjType::Map)
            .unwrap();
        doc.doc.put(&cell_map, "id", cell_id).unwrap();
        doc.doc.put(&cell_map, "cell_type", cell_type).unwrap();
        let source_id = doc
            .doc
            .put_object(&cell_map, "source", ObjType::Text)
            .unwrap();
        if !source.is_empty() {
            doc.doc.splice_text(&source_id, 0, 0, source).unwrap();
        }
        doc.doc.put(&cell_map, "execution_count", "null").unwrap();
        doc.doc
            .put_object(&cell_map, "outputs", ObjType::List)
            .unwrap();
    }

    #[test]
    fn test_migrate_v1_empty_doc() {
        let mut doc = make_v1_doc("nb-empty");
        assert_eq!(doc.schema_version(), Some(1));
        assert_eq!(doc.cell_count(), 0);

        doc.migrate_v1_to_v2().unwrap();

        assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));
        assert_eq!(doc.cell_count(), 0);
        // Should be a Map now — v2 operations should work
        assert!(doc.cells_map_id().is_some());
    }

    #[test]
    fn test_migrate_v1_with_cells() {
        let mut doc = make_v1_doc("nb-migrate");
        v1_add_cell(&mut doc, 0, "cell-a", "code", "print('hello')");
        v1_add_cell(&mut doc, 1, "cell-b", "markdown", "# Title");
        v1_add_cell(&mut doc, 2, "cell-c", "code", "x = 42");
        // cell_count() uses the Map accessor, so it returns 0 for v1 List docs.
        // Verify the List has 3 entries directly.
        let list_id = doc.doc.get(automerge::ROOT, "cells").unwrap().unwrap().1;
        assert_eq!(doc.doc.length(&list_id), 3);

        doc.migrate_v1_to_v2().unwrap();

        assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));
        assert_eq!(doc.cell_count(), 3);

        let cells = doc.get_cells();
        // Order preserved
        assert_eq!(cells[0].id, "cell-a");
        assert_eq!(cells[1].id, "cell-b");
        assert_eq!(cells[2].id, "cell-c");
        // Content preserved
        assert_eq!(cells[0].source, "print('hello')");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[1].source, "# Title");
        assert_eq!(cells[1].cell_type, "markdown");
        assert_eq!(cells[2].source, "x = 42");
        // Positions are strictly increasing
        assert!(cells[0].position < cells[1].position);
        assert!(cells[1].position < cells[2].position);
    }

    #[test]
    fn test_migrate_v1_preserves_execution_count_and_outputs() {
        let mut doc = make_v1_doc("nb-preserve");
        v1_add_cell(&mut doc, 0, "cell-a", "code", "1+1");

        // Manually set execution_count and outputs on the v1 cell
        let cells_id = doc.doc.get(automerge::ROOT, "cells").unwrap().unwrap().1;
        let cell_obj = doc.doc.get(&cells_id, 0usize).unwrap().unwrap().1;
        doc.doc.put(&cell_obj, "execution_count", "5").unwrap();
        // Replace outputs list with one entry
        let _ = doc.doc.delete(&cell_obj, "outputs");
        let outputs_id = doc
            .doc
            .put_object(&cell_obj, "outputs", ObjType::List)
            .unwrap();
        doc.doc
            .insert(&outputs_id, 0, r#"{"output_type":"stream","text":"2\n"}"#)
            .unwrap();

        doc.migrate_v1_to_v2().unwrap();

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].execution_count, "5");
        assert_eq!(cells[0].outputs.len(), 1);
        assert!(cells[0].outputs[0].contains("stream"));
    }

    #[test]
    fn test_migrate_v2_is_noop() {
        let mut doc = NotebookDoc::new("nb-v2");
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "already v2").unwrap();
        assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));

        let cells_before = doc.get_cells();

        // Calling migrate on a v2 doc is a no-op: the early return
        // checks schema_version >= SCHEMA_VERSION before touching anything.
        let result = doc.migrate_v1_to_v2();
        assert!(result.is_ok());

        // Verify cells are unchanged.
        assert_eq!(cells_before, doc.get_cells());
    }

    // ── Native Automerge metadata tests ───────────────────────────────

    #[test]
    fn test_put_get_json_value_all_types() {
        let mut doc = NotebookDoc::new("nb-json-types");
        let meta_id = doc.metadata_map_id().unwrap();

        // Null
        doc.put_json_value(&meta_id, "null_val", &serde_json::Value::Null)
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "null_val"),
            Some(serde_json::Value::Null)
        );

        // Bool
        doc.put_json_value(&meta_id, "bool_val", &serde_json::json!(true))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "bool_val"),
            Some(serde_json::json!(true))
        );

        // Integer (i64)
        doc.put_json_value(&meta_id, "int_val", &serde_json::json!(42))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "int_val"),
            Some(serde_json::json!(42))
        );

        // Negative integer
        doc.put_json_value(&meta_id, "neg_int", &serde_json::json!(-7))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "neg_int"),
            Some(serde_json::json!(-7))
        );

        // Float
        doc.put_json_value(&meta_id, "float_val", &serde_json::json!(3.15))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "float_val"),
            Some(serde_json::json!(3.15))
        );

        // String
        doc.put_json_value(&meta_id, "str_val", &serde_json::json!("hello"))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "str_val"),
            Some(serde_json::json!("hello"))
        );

        // Array with mixed types
        let arr = serde_json::json!([1, "two", null, true, 3.5]);
        doc.put_json_value(&meta_id, "arr_val", &arr).unwrap();
        assert_eq!(doc.get_json_value(&meta_id, "arr_val"), Some(arr));

        // Nested object
        let nested = serde_json::json!({
            "a": 1,
            "b": {"c": [true, false, null]},
            "d": null,
            "e": "string"
        });
        doc.put_json_value(&meta_id, "nested_val", &nested).unwrap();
        assert_eq!(doc.get_json_value(&meta_id, "nested_val"), Some(nested));

        // Empty object and array
        doc.put_json_value(&meta_id, "empty_obj", &serde_json::json!({}))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "empty_obj"),
            Some(serde_json::json!({}))
        );
        doc.put_json_value(&meta_id, "empty_arr", &serde_json::json!([]))
            .unwrap();
        assert_eq!(
            doc.get_json_value(&meta_id, "empty_arr"),
            Some(serde_json::json!([]))
        );

        // Non-existent key returns None
        assert_eq!(doc.get_json_value(&meta_id, "missing"), None);
    }

    #[test]
    fn test_native_metadata_snapshot_round_trip() {
        let mut doc = NotebookDoc::new("nb-native-snap");

        let snapshot = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: Some(metadata::LanguageInfoSnapshot {
                name: "python".to_string(),
                version: Some("3.11.5".to_string()),
            }),
            runt: metadata::RuntMetadata::default(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();
        let read_back = doc.get_metadata_snapshot().unwrap();
        assert_eq!(read_back, snapshot);
    }

    #[test]
    fn test_native_metadata_write() {
        let mut doc = NotebookDoc::new("nb-native-write");

        let snapshot = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        // Native keys should exist
        let meta_id = doc.metadata_map_id().unwrap();
        assert!(doc.get_json_value(&meta_id, "kernelspec").is_some());
        assert!(doc.get_json_value(&meta_id, "runt").is_some());

        // Read back via native keys
        let read_back = doc.get_metadata_snapshot().unwrap();
        assert_eq!(read_back, snapshot);
    }

    #[test]
    fn test_set_get_metadata_value() {
        let mut doc = NotebookDoc::new("nb-meta-val");

        let value = serde_json::json!({"name": "python3", "display_name": "Python 3"});
        doc.set_metadata_value("my_key", &value).unwrap();

        let read_back = doc.get_metadata_value("my_key");
        assert_eq!(read_back, Some(value));

        // Non-existent key
        assert_eq!(doc.get_metadata_value("missing"), None);
    }

    #[test]
    fn test_cell_metadata_native_round_trip() {
        let mut doc = NotebookDoc::new("nb-cell-native-rt");
        doc.add_cell(0, "cell1", "code").unwrap();

        // New cell has empty native map metadata
        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.metadata, serde_json::json!({}));

        // Set complex nested metadata
        let meta = serde_json::json!({
            "jupyter": {"source_hidden": true, "outputs_hidden": false},
            "tags": ["test", "example"],
            "custom": {"nested": {"deep": 42}, "flag": null, "active": true}
        });
        doc.set_cell_metadata("cell1", &meta).unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.metadata, meta);
        assert!(cell.is_source_hidden());
        assert!(!cell.is_outputs_hidden());
        assert_eq!(cell.tags(), vec!["test", "example"]);
    }

    #[test]
    fn test_cell_metadata_native_via_add_cell_full() {
        let mut doc = NotebookDoc::new("nb-full-native");
        let meta = serde_json::json!({
            "jupyter": {"source_hidden": true},
            "tags": ["hide-input"],
            "count": 0,
            "nullable": null
        });
        doc.add_cell_full("cell1", "code", "80", "x = 1", &[], "null", &meta)
            .unwrap();

        let cell = doc.get_cell("cell1").unwrap();
        assert_eq!(cell.metadata, meta);
        assert!(cell.is_source_hidden());
        assert_eq!(cell.tags(), vec!["hide-input"]);
    }

    #[test]
    fn test_cell_metadata_native_sync() {
        use automerge::sync;

        let mut daemon = NotebookDoc::new("nb-native-sync");
        daemon.add_cell(0, "cell1", "code").unwrap();

        let meta = serde_json::json!({
            "jupyter": {"source_hidden": true},
            "tags": ["synced"],
            "custom_val": 99
        });
        daemon.set_cell_metadata("cell1", &meta).unwrap();

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();

        for _ in 0..5 {
            if let Some(msg) = daemon.generate_sync_message(&mut daemon_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                daemon.receive_sync_message(&mut daemon_state, msg).unwrap();
            }
        }

        let cell = client.get_cell("cell1").unwrap();
        assert_eq!(cell.metadata, meta);
        assert!(cell.is_source_hidden());
        assert_eq!(cell.tags(), vec!["synced"]);
    }

    #[test]
    fn test_get_cells_from_doc_native_metadata() {
        let mut doc = NotebookDoc::new("nb-free-fn");
        let meta = serde_json::json!({
            "jupyter": {"source_hidden": true},
            "tags": ["from-doc"]
        });
        doc.add_cell_full("cell1", "code", "80", "x = 1", &[], "null", &meta)
            .unwrap();

        // Use the free function (as sync client would)
        let cells = get_cells_from_doc(doc.doc());
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].metadata, meta);
    }

    #[test]
    fn test_migrate_v1_cells_usable_after_migration() {
        let mut doc = make_v1_doc("nb-usable");
        v1_add_cell(&mut doc, 0, "old-cell", "code", "original");

        doc.migrate_v1_to_v2().unwrap();

        // Can add new cells after migration
        doc.add_cell_after("new-cell", "code", Some("old-cell"))
            .unwrap();
        let cells = doc.get_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "old-cell");
        assert_eq!(cells[1].id, "new-cell");

        // Can move cells after migration
        doc.move_cell("new-cell", None).unwrap();
        let cells = doc.get_cells();
        assert_eq!(cells[0].id, "new-cell");
        assert_eq!(cells[1].id, "old-cell");

        // Can update source after migration
        doc.update_source("old-cell", "updated").unwrap();
        assert_eq!(doc.get_cell("old-cell").unwrap().source, "updated");

        // Can delete after migration
        doc.delete_cell("new-cell").unwrap();
        assert_eq!(doc.cell_count(), 1);
    }

    // ── Actor provenance tests ──────────────────────────────────────────

    #[test]
    fn test_set_actor_identity() {
        let mut doc = NotebookDoc::new("test");
        doc.set_actor("runtimed");
        assert_eq!(doc.get_actor_id(), "runtimed");
    }

    #[test]
    fn test_new_with_actor() {
        let doc = NotebookDoc::new_with_actor("test", "agent:claude:abc123");
        assert_eq!(doc.get_actor_id(), "agent:claude:abc123");
    }

    #[test]
    fn test_empty_with_actor() {
        let doc = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:session-1");
        assert_eq!(doc.get_actor_id(), "human:session-1");
    }

    #[test]
    fn test_actor_survives_sync() {
        use automerge::sync;

        // runtimed doc with "runtimed" actor
        let mut runtimed = NotebookDoc::new_with_actor("test-notebook", "runtimed");
        runtimed.add_cell(0, "cell-1", "code").unwrap();

        // Frontend doc with "human" actor
        let mut frontend = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:tab-1");

        let mut runtimed_sync = sync::State::new();
        let mut frontend_state = sync::State::new();

        // Sync until convergence
        for _ in 0..10 {
            if let Some(msg) = runtimed.generate_sync_message(&mut runtimed_sync) {
                frontend
                    .receive_sync_message(&mut frontend_state, msg)
                    .unwrap();
            }
            if let Some(msg) = frontend.generate_sync_message(&mut frontend_state) {
                runtimed
                    .receive_sync_message(&mut runtimed_sync, msg)
                    .unwrap();
            }
        }

        // Both docs have the cell
        assert_eq!(frontend.cell_count(), 1);

        // Actor identities are preserved after sync
        assert_eq!(runtimed.get_actor_id(), "runtimed");
        assert_eq!(frontend.get_actor_id(), "human:tab-1");

        // Frontend makes an edit — tagged with its own actor
        frontend
            .update_source("cell-1", "# edited by human")
            .unwrap();

        // Sync the edit back
        for _ in 0..10 {
            if let Some(msg) = frontend.generate_sync_message(&mut frontend_state) {
                runtimed
                    .receive_sync_message(&mut runtimed_sync, msg)
                    .unwrap();
            }
            if let Some(msg) = runtimed.generate_sync_message(&mut runtimed_sync) {
                frontend
                    .receive_sync_message(&mut frontend_state, msg)
                    .unwrap();
            }
        }

        // runtimed sees the edit
        assert_eq!(
            runtimed.get_cell("cell-1").unwrap().source,
            "# edited by human"
        );
    }

    #[test]
    fn test_default_actor_is_random_hex() {
        let doc = NotebookDoc::new("test");
        let actor_id = doc.get_actor_id();
        // Default actor is a random UUID (32 hex chars)
        // get_actor_id falls back to hex for non-UTF-8 bytes
        assert!(!actor_id.is_empty());
    }

    #[test]
    fn test_contributing_actors_single() {
        let mut doc = NotebookDoc::new_with_actor("test", "runtimed");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let actors = doc.contributing_actors();
        assert_eq!(actors, vec!["runtimed"]);
    }

    #[test]
    fn test_contributing_actors_after_sync() {
        use automerge::sync;

        // runtimed creates the doc and adds a cell
        let mut runtimed = NotebookDoc::new_with_actor("nb", "runtimed");
        runtimed.add_cell(0, "cell-1", "code").unwrap();

        // human joins and syncs
        let mut human = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:tab-1");
        let mut rs = sync::State::new();
        let mut hs = sync::State::new();
        for _ in 0..10 {
            if let Some(msg) = runtimed.generate_sync_message(&mut rs) {
                human.receive_sync_message(&mut hs, msg).unwrap();
            }
            if let Some(msg) = human.generate_sync_message(&mut hs) {
                runtimed.receive_sync_message(&mut rs, msg).unwrap();
            }
        }

        // human edits
        human.update_source("cell-1", "print('hello')").unwrap();

        // sync back
        for _ in 0..10 {
            if let Some(msg) = human.generate_sync_message(&mut hs) {
                runtimed.receive_sync_message(&mut rs, msg).unwrap();
            }
            if let Some(msg) = runtimed.generate_sync_message(&mut rs) {
                human.receive_sync_message(&mut hs, msg).unwrap();
            }
        }

        // Both docs see both contributors
        let actors = runtimed.contributing_actors();
        assert_eq!(actors, vec!["human:tab-1", "runtimed"]);

        let actors = human.contributing_actors();
        assert_eq!(actors, vec!["human:tab-1", "runtimed"]);
    }

    /// Validates the local-first empty notebook flow: daemon creates a doc
    /// with metadata but zero cells, frontend creates a cell locally, then
    /// sync converges so both sides have the cell.
    #[test]
    fn test_frontend_creates_cell_syncs_to_empty_daemon() {
        use automerge::sync;

        // Daemon creates doc with metadata but 0 cells
        let mut daemon = NotebookDoc::new_with_actor("nb", "runtimed");
        assert_eq!(daemon.cell_count(), 0);

        // Frontend starts empty
        let mut frontend = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:tab-1");

        let mut ds = sync::State::new();
        let mut fs = sync::State::new();

        // Initial sync: frontend gets daemon's schema/metadata
        for _ in 0..10 {
            if let Some(m) = daemon.generate_sync_message(&mut ds) {
                frontend.receive_sync_message(&mut fs, m).unwrap();
            }
            if let Some(m) = frontend.generate_sync_message(&mut fs) {
                daemon.receive_sync_message(&mut ds, m).unwrap();
            }
        }
        assert_eq!(frontend.cell_count(), 0);
        assert_eq!(daemon.cell_count(), 0);

        // Frontend creates a cell locally (like the autoseed effect)
        frontend.add_cell(0, "cell-1", "code").unwrap();
        assert_eq!(frontend.cell_count(), 1);

        // Sync again — frontend's cell should reach the daemon
        for _ in 0..10 {
            if let Some(m) = frontend.generate_sync_message(&mut fs) {
                daemon.receive_sync_message(&mut ds, m).unwrap();
            }
            if let Some(m) = daemon.generate_sync_message(&mut ds) {
                frontend.receive_sync_message(&mut fs, m).unwrap();
            }
        }

        // Both should have the cell
        assert_eq!(daemon.cell_count(), 1);
        assert_eq!(frontend.cell_count(), 1);
        assert_eq!(daemon.get_cells()[0].id, "cell-1");
    }

    #[test]
    fn test_per_cell_accessors() {
        let mut doc = NotebookDoc::new("nb-accessors");

        // Add cells in a specific order: cell-b first, then cell-a before it, then cell-c after cell-b
        doc.add_cell_after("cell-b", "code", None).unwrap();
        doc.add_cell_after("cell-a", "markdown", None).unwrap();
        doc.add_cell_after("cell-c", "raw", Some("cell-b")).unwrap();

        // Set some source content
        doc.update_source("cell-a", "# Title").unwrap();
        doc.update_source("cell-b", "print('hello')").unwrap();
        doc.update_source("cell-c", "raw content").unwrap();

        // Verify get_cell_ids returns IDs in position order: a, b, c
        let ids = doc.get_cell_ids();
        assert_eq!(ids, vec!["cell-a", "cell-b", "cell-c"]);

        // Verify per-cell source
        assert_eq!(doc.get_cell_source("cell-a"), Some("# Title".to_string()));
        assert_eq!(
            doc.get_cell_source("cell-b"),
            Some("print('hello')".to_string())
        );
        assert_eq!(
            doc.get_cell_source("cell-c"),
            Some("raw content".to_string())
        );

        // Verify per-cell type
        assert_eq!(doc.get_cell_type("cell-a"), Some("markdown".to_string()));
        assert_eq!(doc.get_cell_type("cell-b"), Some("code".to_string()));
        assert_eq!(doc.get_cell_type("cell-c"), Some("raw".to_string()));

        // Verify execution count defaults
        assert_eq!(
            doc.get_cell_execution_count("cell-b"),
            Some("null".to_string())
        );

        // Verify outputs default to empty vec
        assert_eq!(doc.get_cell_outputs("cell-b"), Some(vec![]));

        // Verify metadata default to empty object
        assert_eq!(doc.get_cell_metadata("cell-a"), Some(serde_json::json!({})));

        // Verify position is present
        assert!(doc.get_cell_position("cell-a").is_some());
        assert!(doc.get_cell_position("cell-b").is_some());

        // Verify nonexistent cell returns None for all accessors
        assert_eq!(doc.get_cell_source("nonexistent"), None);
        assert_eq!(doc.get_cell_type("nonexistent"), None);
        assert_eq!(doc.get_cell_outputs("nonexistent"), None);
        assert_eq!(doc.get_cell_execution_count("nonexistent"), None);
        assert_eq!(doc.get_cell_metadata("nonexistent"), None);
        assert_eq!(doc.get_cell_position("nonexistent"), None);
    }

    #[test]
    fn test_metadata_fingerprint_stable_when_unchanged() {
        let mut doc = NotebookDoc::new("nb-fp-stable");

        let snapshot = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        let fp1 = doc.get_metadata_fingerprint();
        let fp2 = doc.get_metadata_fingerprint();
        assert!(fp1.is_some());
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_metadata_fingerprint_changes_on_metadata_update() {
        let mut doc = NotebookDoc::new("nb-fp-change");

        let snapshot = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        let fp_before = doc.get_metadata_fingerprint().unwrap();

        doc.add_uv_dependency("pandas>=2.0").unwrap();

        let fp_after = doc.get_metadata_fingerprint().unwrap();
        assert_ne!(fp_before, fp_after);
    }

    #[test]
    fn test_metadata_fingerprint_stable_across_cell_changes() {
        let mut doc = NotebookDoc::new("nb-fp-cells");

        let snapshot = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        let fp_before = doc.get_metadata_fingerprint().unwrap();

        doc.add_cell_after("cell-1", "code", None).unwrap();
        doc.update_source("cell-1", "print('hello')").unwrap();

        let fp_after = doc.get_metadata_fingerprint().unwrap();
        assert_eq!(fp_before, fp_after);
    }

    #[test]
    fn test_stream_upsert_fork_merge_with_concurrent_clear() {
        // Verifies that when a fork performs upsert_stream_output and the main
        // doc concurrently clears outputs, merge composes correctly. The fork's
        // index is the correct one to cache — if a concurrent clear invalidates
        // it, the next stream chunk safely falls back to append.
        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();

        // Append an initial stream output so there's something to upsert
        let initial_hash = "sha256:aaa";
        doc.append_output("cell-1", initial_hash).unwrap();
        assert_eq!(doc.get_cell_outputs("cell-1").unwrap().len(), 1);

        let known_state = StreamOutputState {
            index: 0,
            manifest_hash: initial_hash.to_string(),
        };

        // Fork before the "async work"
        let mut fork = doc.fork();
        fork.set_actor("runtimed:kernel");

        // Upsert on the fork (updates in place since known_state matches)
        let new_hash = "sha256:bbb";
        let (updated, fork_index) = fork
            .upsert_stream_output("cell-1", "stdout", new_hash, Some(&known_state))
            .unwrap();
        assert!(updated, "should update in place on fork");
        assert_eq!(fork_index, 0, "fork index is where the upsert wrote");

        // Concurrent mutation on main doc: clear all outputs
        doc.clear_outputs("cell-1").unwrap();
        assert_eq!(doc.get_cell_outputs("cell-1").unwrap().len(), 0);

        // Merge fork back — CRDT composes the clear with the upsert
        doc.merge(&mut fork).unwrap();

        // After merge: the fork did a `put` (in-place update) on an element
        // that the main doc deleted. Automerge's semantics: a put on a
        // deleted element is lost — the concurrent clear wins.
        let outputs = doc.get_cell_outputs("cell-1").unwrap();
        assert_eq!(
            outputs.len(),
            0,
            "concurrent clear wins over fork's in-place put"
        );

        // The cached fork_index (0) is now stale, but that's safe:
        // the next upsert_stream_output call will see output_count=0,
        // validation will fail (index 0 >= output_count 0), and it
        // will fall back to appending a fresh entry.
        assert_eq!(fork_index, 0, "fork index is stale but harmless");
    }

    #[test]
    fn test_stream_upsert_fork_merge_append_case() {
        // Verifies that fork+merge works for the append case (no known state)
        // and that the fork's index is correct for terminal state caching.
        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();

        // Fork before the "async work"
        let mut fork = doc.fork();
        fork.set_actor("runtimed:kernel");

        // Upsert with no known state — this appends
        let hash = "sha256:ccc";
        let (updated, fork_index) = fork
            .upsert_stream_output("cell-1", "stdout", hash, None)
            .unwrap();
        assert!(!updated, "should append, not update");
        assert_eq!(fork_index, 0, "fork appended at position 0");

        // Concurrently append a different output on main doc
        doc.append_output("cell-1", "sha256:other").unwrap();

        // Merge
        doc.merge(&mut fork).unwrap();

        // Both outputs should be present
        let outputs = doc.get_cell_outputs("cell-1").unwrap();
        assert_eq!(
            outputs.len(),
            2,
            "both concurrent appends should survive merge"
        );

        // The fork's index (0) is correct for caching — it points to the
        // fork's output entry. Using len()-1 would be wrong here since
        // the fork's output may not be the last entry after merge.
        assert!(
            outputs.contains(&hash.to_string()),
            "fork's appended output should be in merged doc"
        );
    }
}
