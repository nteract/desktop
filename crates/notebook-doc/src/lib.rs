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
//! ## Document schema (v4)
//!
//! Outputs live in `RuntimeStateDoc` (keyed by `execution_id`) with
//! per-output `output_id` UUIDs on their manifests — they are not stored
//! in the notebook doc itself.
//!
//! ```text
//! ROOT/
//!   schema_version: u64           ← Document schema version (currently 4)
//!   notebook_id: Str
//!   cells/                        ← Map keyed by cell ID (O(1) lookup)
//!     {cell_id}/
//!       id: Str                   ← cell UUID (redundant but convenient)
//!       cell_type: Str            ← "code" | "markdown" | "raw"
//!       position: Str             ← Fractional index hex string for ordering
//!       source: Text              ← Automerge Text CRDT (character-level merging)
//!       execution_count: Str      ← Legacy nbformat/import-export fallback;
//!                                   live counts are in RuntimeStateDoc
//!       metadata/                 ← Map (native Automerge types, legacy: JSON string fallback)
//!       resolved_assets/          ← Map of markdown asset ref -> blob hash
//!   metadata/                     ← Map
//!     runtime: Str
//!     kernelspec/                 ← Map (native Automerge, per-field CRDT merge)
//!     language_info/              ← Map (native Automerge, per-field CRDT merge)
//!     runt/                       ← Map (native Automerge, per-field CRDT merge)
//!     notebook_metadata: Str      ← Legacy JSON string (backward compat, dual-written)
//! ```

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod diff;
pub mod frame_types;
pub mod metadata;
pub mod mime;
pub mod pep723;
pub mod pool_state;
pub mod presence;
use std::collections::HashMap;

/// Current document schema version.
///
/// Bump this when making incompatible changes to the Automerge document
/// structure. Future bumps MUST ship a matching `migrate_vN_to_v(N+1)`
/// function that preserves user data — see the "one-time cleanup"
/// comment in `load_or_create_inner` for why the current fallback path
/// is not a template.
///
/// History:
/// - **1** — Original schema: `cells` is an ordered `List` of `Map`.
/// - **2** — Fractional indexing: `cells` is a `Map` keyed by cell ID, each cell has a `position` field.
/// - **3** — Outputs moved to RuntimeStateDoc: cell outputs are no longer stored in the notebook doc.
/// - **4** — Addressable outputs: `OutputManifest` carries a required `output_id` (UUIDv4).
///   Outputs live in RuntimeStateDoc keyed by `execution_id`; manifests carry `output_id`.
///
/// v1–v2 predate the nteract 2.0 pre-release series and are no longer
/// supported. `load_or_create_inner` discards pre-v3 documents on load.
/// v3 documents are migrated in-place (version bump only).
pub const SCHEMA_VERSION: u64 = 4;

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

/// Snapshot of a single cell's state, suitable for serialization.
///
/// `CellSnapshot` represents only the fields that live in the notebook
/// Automerge document. Outputs moved to `RuntimeStateDoc` in schema v3
/// and are looked up separately, keyed by the cell's `execution_id`.
/// Callers that need outputs should use a dedicated lookup such as
/// `DocHandle::get_cell_outputs(cell_id)` or `DocHandle::get_all_outputs()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CellSnapshot {
    pub id: String,
    /// "code", "markdown", or "raw"
    pub cell_type: String,
    /// Fractional index hex string for ordering (e.g., "80", "7F80").
    /// Cells are sorted lexicographically by this field.
    pub position: String,
    pub source: String,
    /// Legacy JSON-encoded execution count: a number string like "5" or "null".
    ///
    /// Live execution counts are daemon-authored in `RuntimeStateDoc`. This
    /// field preserves nbformat/import-export history when runtime state is
    /// unavailable.
    pub execution_count: String,
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

    /// Fork the document and set a distinct actor ID on the fork in one step.
    ///
    /// Forks inherit the parent's actor ID, and Automerge tracks ops by
    /// `(actor, seq)`. Two concurrent forks that share an actor will each
    /// produce ops at seq `N`, `N+1`, …; the first merge lands and the
    /// second returns `DuplicateSeqNumber` — silently dropping writes if
    /// the error is ignored. See the regression test
    /// `merging_two_forks_with_shared_actor_returns_duplicate_seq_error`
    /// in `runtime_state.rs`.
    ///
    /// Use this for any fork whose merge crosses an `.await` point. The
    /// `actor` argument is set verbatim — the caller controls whether
    /// the actor is stable per task (e.g. `"rt:kernel:abc:iopub"`) or
    /// unique per fork (e.g. `format!("runtimed:assets:{}", Uuid::new_v4())`).
    ///
    /// Prefer a stable per-task actor for long-running loops that fork
    /// many times in sequence — each unique actor consumes space in
    /// Automerge's internal actor list. Use a UUID suffix for one-shot
    /// sites where concurrent forks from the same logical task can
    /// overlap across the async gap.
    ///
    /// For synchronous fork+merge blocks, use
    /// [`fork_and_merge`](Self::fork_and_merge) — actor collisions are
    /// harmless there because the merge completes before any other fork
    /// of the same parent can exist.
    pub fn fork_with_actor(&mut self, actor: impl AsRef<str>) -> Self {
        let mut fork = self.fork();
        fork.set_actor(actor.as_ref());
        fork
    }

    /// Get the current document heads (change hashes at the tip).
    pub fn get_heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// Get the current document heads as hex strings for JS/protocol guards.
    pub fn get_heads_hex(&mut self) -> Vec<String> {
        self.get_heads()
            .into_iter()
            .map(|head| head.to_string())
            .collect()
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
        update_json_at_key(&mut self.doc, parent, key, value)
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
        update_json_at_key(&mut self.doc, &meta_id, key, value)
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

        let extras = scan_metadata_extras(&self.doc, &meta_id);

        if kernelspec.is_some() || language_info.is_some() || runt.is_some() || !extras.is_empty() {
            return Some(metadata::NotebookMetadataSnapshot {
                kernelspec,
                language_info,
                runt: runt.unwrap_or_default(),
                extras,
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
                update_json_at_key(&mut self.doc, &meta_id, "kernelspec", &v)?;
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
                update_json_at_key(&mut self.doc, &meta_id, "language_info", &v)?;
            }
            None => {
                let _ = self.doc.delete(&meta_id, "language_info");
            }
        }

        // Write runt only when non-empty so vanilla Jupyter notebooks
        // round-trip without stamping a synthetic runt blob.
        if snapshot.runt.is_empty() {
            let _ = self.doc.delete(&meta_id, "runt");
        } else {
            let runt_v = serde_json::to_value(&snapshot.runt)
                .map_err(|e| AutomergeError::InvalidObjId(format!("serialize runt: {}", e)))?;
            update_json_at_key(&mut self.doc, &meta_id, "runt", &runt_v)?;
        }

        // Delete extras keys that were present in the doc but are absent
        // from the incoming snapshot. This is the replacement semantic
        // the file-watcher path needs: when a user deletes `jupytext`
        // from the .ipynb, re-parses it, and pushes the new snapshot
        // back, the old Automerge map must go. Without this, stale
        // extras linger in the doc forever.
        //
        // Scan before writing so we don't delete keys we're about to
        // re-add (a no-op) and so reserved keys (kernelspec,
        // language_info, runt, runtime, ephemeral) never get touched.
        let stale_keys: Vec<String> = self
            .doc
            .keys(&meta_id)
            .filter(|k| !is_snapshot_reserved_metadata_key(k))
            .filter(|k| !snapshot.extras.contains_key(k))
            .collect();
        for key in &stale_keys {
            let _ = self.doc.delete(&meta_id, key.as_str());
        }

        // Write extras. Each key becomes its own Automerge Map so
        // concurrent edits to metadata.jupytext.* from two peers merge
        // per-field. Guard against callers that stuff known typed keys
        // into extras — those would double-write at the same Automerge
        // key. The `runtime`/`ephemeral` reserved scalars aren't in
        // this guard because they're already skipped on the read path;
        // if a caller did stash one in extras, silently dropping it in
        // the stale-key filter below is preferable to an error log.
        for (key, value) in &snapshot.extras {
            if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
                report_extras_collision(key);
                continue;
            }
            if is_snapshot_reserved_metadata_key(key) {
                continue;
            }
            update_json_at_key(&mut self.doc, &meta_id, key, value)?;
        }

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

    /// Return a stable fingerprint of dependency metadata covered by trust approval.
    pub fn get_dependency_fingerprint(&self) -> Option<String> {
        Some(self.get_metadata_snapshot()?.dependency_fingerprint())
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

    // ── Pixi dependency operations ──────────────────────────────────

    /// Add a Pixi conda dependency (matchspec). Deduplicates by package name.
    pub fn add_pixi_dependency(&mut self, pkg: &str) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.add_pixi_dependency(pkg);
        self.set_metadata_snapshot(&snapshot)
    }

    /// Remove a Pixi conda dependency by package name.
    pub fn remove_pixi_dependency(&mut self, pkg: &str) -> Result<bool, AutomergeError> {
        let Some(mut snapshot) = self.get_metadata_snapshot() else {
            return Ok(false);
        };
        let removed = snapshot.remove_pixi_dependency(pkg);
        if removed {
            self.set_metadata_snapshot(&snapshot)?;
        }
        Ok(removed)
    }

    /// Clear the Pixi section entirely.
    pub fn clear_pixi_section(&mut self) -> Result<(), AutomergeError> {
        if let Some(mut snapshot) = self.get_metadata_snapshot() {
            snapshot.clear_pixi_section();
            self.set_metadata_snapshot(&snapshot)
        } else {
            Ok(())
        }
    }

    /// Set Pixi channels, preserving deps.
    pub fn set_pixi_channels(&mut self, channels: Vec<String>) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_pixi_channels(channels);
        self.set_metadata_snapshot(&snapshot)
    }

    /// Set Pixi python version.
    pub fn set_pixi_python(&mut self, python: Option<String>) -> Result<(), AutomergeError> {
        let mut snapshot = self.get_metadata_snapshot().unwrap_or_default();
        snapshot.set_pixi_python(python);
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

    /// Create a client-side bootstrap document for sync.
    ///
    /// Every client — WASM frontend, Python bindings, future Swift, etc. —
    /// must start from the same document skeleton before syncing with the
    /// daemon.  The skeleton mirrors what [`NotebookDoc::new()`] creates:
    ///
    /// ```text
    /// ROOT/
    ///   schema_version: <SCHEMA_VERSION>  (scalar only — no cells/metadata maps)
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
    /// The daemon creates these maps in `new_inner()`. We must NOT create
    /// `cells` or `metadata` maps here — if we do, Automerge sees two
    /// concurrent `put_object` operations at the same key from different
    /// actors, creating a conflict. If our empty map wins, the daemon's
    /// populated map (with cells or dependencies) becomes invisible.
    pub fn bootstrap(encoding: TextEncoding, actor_label: &str) -> Self {
        let mut doc = AutoCommit::new_with_encoding(encoding);
        doc.set_actor(ActorId::from(actor_label.as_bytes()));

        // Seed with a single scalar so Automerge's is_empty() returns false.
        // This prevents load_incremental's empty-doc fast-path from replacing
        // *self with a default-options doc, which would discard our encoding
        // and actor settings. The daemon's cells and metadata maps arrive
        // via normal Automerge sync — no need to create them here.
        let _ = doc.put(automerge::ROOT, "schema_version", SCHEMA_VERSION);

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
                        if version == SCHEMA_VERSION {
                            info!("[notebook-doc] Loaded from {:?} for {}", path, notebook_id);
                            if let Some(label) = actor_label {
                                loaded.set_actor(label);
                            }
                            return loaded;
                        }

                        // v3 → v4: output_id was added to OutputManifest, but
                        // it's minted at capture time (#[serde(default)]), so
                        // the migration is a version-bump no-op.
                        if version == 3 {
                            info!(
                                "[notebook-doc] Migrating schema v3 → v{} for {} at {:?}",
                                SCHEMA_VERSION, notebook_id, path
                            );
                            let _ =
                                loaded
                                    .doc
                                    .put(automerge::ROOT, "schema_version", SCHEMA_VERSION);
                            if let Some(label) = actor_label {
                                loaded.set_actor(label);
                            }
                            return loaded;
                        }

                        // v1–v2 predate nteract 2.0 and use incompatible cell
                        // schemas (ordered List vs fractional-indexed Map).
                        // Preserve the file for manual recovery, then start fresh.
                        warn!(
                            "[notebook-doc] Rejecting schema v{} notebook at {:?} for {}; \
                             migration is only supported from v3. \
                             Preserving as .corrupt and starting fresh.",
                            version, path, notebook_id
                        );
                        Self::preserve_corrupt(path);
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
    ///
    /// Includes a defensive cell-count guard: if the rebuilt doc would have
    /// fewer cells, the rebuild is skipped to prevent silent cell loss when
    /// `save()` on a panic-corrupted doc drops ops from the serialized bytes.
    pub fn rebuild_from_save(&mut self) -> bool {
        let actor = self.doc.get_actor().clone();
        let pre_cell_count = self.cell_count();
        let bytes = self.doc.save();
        match AutoCommit::load(&bytes) {
            Ok(mut doc) => {
                let post_cell_count = get_cells_from_doc(&doc).len();
                if post_cell_count < pre_cell_count {
                    #[cfg(feature = "persistence")]
                    warn!(
                        "[notebook-doc] rebuild_from_save would lose cells ({} → {}), skipping",
                        pre_cell_count, post_cell_count
                    );
                    return false;
                }
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

    /// Get cells at a historical document head set for guard comparison.
    ///
    /// Populates the fields needed to compare executable cell identity,
    /// ordering, type, source, and execution count. Cell metadata and resolved
    /// markdown assets are intentionally left empty.
    pub fn get_cells_at_heads(&self, heads: &[automerge::ChangeHash]) -> Vec<CellSnapshot> {
        let cells_id = match self.cells_map_id_at(heads) {
            Some(id) => id,
            None => return vec![],
        };

        let mut cells: Vec<CellSnapshot> = self
            .doc
            .keys_at(&cells_id, heads)
            .filter_map(|key| {
                let cell_obj = self.cell_obj_id_at(&cells_id, &key, heads)?;
                self.read_cell_at(&cell_obj, heads)
            })
            .collect();

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

    /// Get the persisted legacy execution count for a cell (O(1) lookup).
    ///
    /// This reads only the notebook document fallback used for nbformat
    /// import/export and reloads without runtime state. Live execution counts
    /// are RuntimeStateDoc-owned and should be read through higher-level
    /// handles that can consult both documents.
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
        self.doc.put_object(&cell_map, "metadata", ObjType::Map)?;
        self.doc
            .put_object(&cell_map, "resolved_assets", ObjType::Map)?;

        Ok(position_str)
    }

    /// Insert a fully-populated cell with an explicit position string.
    ///
    /// `execution_count` is the persisted nbformat/import-export fallback. Live
    /// execution counts are stored in RuntimeStateDoc.
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
    pub fn add_cell_full(
        &mut self,
        cell_id: &str,
        cell_type: &str,
        position: &str,
        source: &str,
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

        // Store metadata as native Automerge map
        // Safe to use put_json_at_key here: meta_map was just created by put_object
        // above, so no other peer can have a competing object at this key.
        let meta_map = self.doc.put_object(&cell_map, "metadata", ObjType::Map)?;
        if let Some(obj) = metadata.as_object() {
            for (k, v) in obj {
                #[allow(deprecated)]
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

    /// Read the execution_id pointer from a cell at a historical head set, if set.
    pub fn get_execution_id_at_heads(
        &self,
        cell_id: &str,
        heads: &[automerge::ChangeHash],
    ) -> Option<String> {
        let cells_id = self.cells_map_id_at(heads)?;
        let cell_obj = self.cell_obj_id_at(&cells_id, cell_id, heads)?;
        read_str_at(&self.doc, &cell_obj, "execution_id", heads)
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

        // Safe to use put_json_at_key: meta_map is freshly created by put_object
        // (replaces entire cell metadata — this is a set_cell_metadata operation).
        let meta_map = self.doc.put_object(&cell_obj, "metadata", ObjType::Map)?;
        if let Some(obj) = metadata.as_object() {
            for (k, v) in obj {
                #[allow(deprecated)]
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

        // Navigate to the parent of the target key, creating objects as needed.
        // Each hop: coerce `current` into a Map, then bind a mutable reference
        // to the child entry (insert `{}` if missing). This replaces the prior
        // unwrap chain with Map-returning pattern matches that make the
        // never-None invariant hold by construction.
        let mut current = &mut metadata;
        for key in &path[..path.len() - 1] {
            if !current.is_object() {
                *current = serde_json::json!({});
            }
            let Some(obj) = current.as_object_mut() else {
                // Unreachable: we just assigned a Map above if it wasn't one.
                unreachable!("current was coerced into a JSON object on the line above");
            };
            current = obj
                .entry((*key).to_string())
                .or_insert_with(|| serde_json::json!({}));
        }

        // Set the final key
        if !current.is_object() {
            *current = serde_json::json!({});
        }
        let final_key = path[path.len() - 1];
        let Some(obj) = current.as_object_mut() else {
            unreachable!("current was coerced into a JSON object on the line above");
        };
        obj.insert(final_key.to_string(), value);

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

    /// Delete a metadata key. Returns `true` if the key existed and was removed.
    pub fn delete_metadata(&mut self, key: &str) -> Result<bool, AutomergeError> {
        let meta_id = match self.metadata_map_id() {
            Some(id) => id,
            None => return Ok(false),
        };
        match self.doc.get(&meta_id, key)? {
            Some(_) => {
                self.doc.delete(&meta_id, key)?;
                Ok(true)
            }
            None => Ok(false),
        }
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

    /// Look up a cell's ObjId by cell ID (two-step: cells map → cell map).
    pub fn cell_obj_for(&self, cell_id: &str) -> Option<ObjId> {
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

    fn text_id_at(
        &self,
        parent: &ObjId,
        key: &str,
        heads: &[automerge::ChangeHash],
    ) -> Option<ObjId> {
        self.doc
            .get_at(parent, key, heads)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Text) => Some(id),
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

    fn cells_map_id_at(&self, heads: &[automerge::ChangeHash]) -> Option<ObjId> {
        self.doc
            .get_at(automerge::ROOT, "cells", heads)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn cell_obj_id_at(
        &self,
        cells_id: &ObjId,
        cell_id: &str,
        heads: &[automerge::ChangeHash],
    ) -> Option<ObjId> {
        self.doc
            .get_at(cells_id, cell_id, heads)
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

        // Outputs live in RuntimeStateDoc, keyed by execution_id. Callers
        // that need them must fetch explicitly (see DocHandle::get_cell_outputs).

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
            metadata,
            resolved_assets,
        })
    }

    fn read_cell_at(
        &self,
        cell_obj: &ObjId,
        heads: &[automerge::ChangeHash],
    ) -> Option<CellSnapshot> {
        let id = read_str_at(&self.doc, cell_obj, "id", heads)?;
        let cell_type = read_str_at(&self.doc, cell_obj, "cell_type", heads).unwrap_or_default();
        let position =
            read_str_at(&self.doc, cell_obj, "position", heads).unwrap_or_else(|| "80".to_string());
        let execution_count = read_str_at(&self.doc, cell_obj, "execution_count", heads)
            .unwrap_or_else(|| "null".to_string());

        let source = self
            .text_id_at(cell_obj, "source", heads)
            .and_then(|text_id| self.doc.text_at(&text_id, heads).ok())
            .unwrap_or_default();

        Some(CellSnapshot {
            id,
            cell_type,
            position,
            source,
            execution_count,
            metadata: serde_json::json!({}),
            resolved_assets: HashMap::new(),
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

fn read_str_at<O: AsRef<automerge::ObjId>, P: Into<automerge::Prop>>(
    doc: &AutoCommit,
    obj: O,
    prop: P,
    heads: &[automerge::ChangeHash],
) -> Option<String> {
    doc.get_at(obj, prop, heads)
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

// JSON/Automerge helpers - single source of truth in automunge crate.
#[allow(deprecated)]
pub use automunge::{
    insert_json_at_index, put_json_at_key, read_json_value, update_json_at_index,
    update_json_at_key,
};

/// Report a metadata-extras collision with a known typed key.
///
/// Called by `NotebookDoc::set_metadata_snapshot` when a caller stuffs
/// a reserved key (`kernelspec`, `language_info`, `runt`) into the
/// extras bag. Dropping the write is safe — the typed field carries
/// the real value — but it indicates a caller bug that would otherwise
/// silently cause an Automerge double-write at the same key. Error
/// level so it surfaces on stable channels too.
///
/// Uses `log` under the `persistence` feature (daemon, native builds)
/// and `eprintln!` otherwise (WASM, where a plain stderr message is
/// the best we can do without pulling in a web-sys dep).
fn report_extras_collision(key: &str) {
    let msg = format!(
        "[notebook-doc] metadata.extras collision: key {:?} is reserved for \
         a typed field; dropping to avoid Automerge double-write. This \
         indicates a caller bug in snapshot construction.",
        key
    );
    #[cfg(feature = "persistence")]
    log::error!("{}", msg);
    #[cfg(not(feature = "persistence"))]
    eprintln!("{}", msg);
}

/// Keys that live at `metadata.*` in the Automerge doc but must NOT be
/// round-tripped as extras on the NotebookMetadataSnapshot:
///
/// - `kernelspec`, `language_info`, `runt`: modeled by typed fields.
/// - `runtime`: an nteract-internal scalar (set by bootstrap,
///   mutated by `set_metadata`) used for runtime-type detection.
///   It's a schema artifact, not an nbformat key, so it must not
///   surface on disk via the extras save path.
/// - `ephemeral`: an nteract-internal scalar marking in-memory-only
///   rooms.
fn is_snapshot_reserved_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "kernelspec" | "language_info" | "runt" | "runtime" | "ephemeral"
    )
}

/// Scan an Automerge metadata Map for top-level keys that aren't modeled
/// by `NotebookMetadataSnapshot`'s typed fields.
///
/// Shared by `NotebookDoc::get_metadata_snapshot` and the free-function
/// `get_metadata_snapshot_from_doc`. Both must behave identically —
/// different behavior would mean the frontend sync snapshot and Python
/// bindings disagree with the daemon's view of the same doc.
fn scan_metadata_extras(
    doc: &AutoCommit,
    meta_id: &ObjId,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut extras = std::collections::BTreeMap::new();
    for key in doc.keys(meta_id) {
        if is_snapshot_reserved_metadata_key(&key) {
            continue;
        }
        if let Some(value) = read_json_value(doc, meta_id, &key) {
            extras.insert(key, value);
        }
    }
    extras
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

    let extras = scan_metadata_extras(doc, &meta_id);

    if kernelspec.is_some() || language_info.is_some() || runt.is_some() || !extras.is_empty() {
        return Some(metadata::NotebookMetadataSnapshot {
            kernelspec,
            language_info,
            runt: runt.unwrap_or_default(),
            extras,
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

            // Outputs live in RuntimeStateDoc, keyed by execution_id.

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
mod tests {
    use super::*;

    #[test]
    fn test_empty_doc_has_bootstrap_skeleton() {
        // bootstrap() seeds only schema_version — no cells or metadata maps.
        // Those are created by the daemon and arrive via Automerge sync.
        let doc = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        assert_eq!(doc.notebook_id(), None);
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(doc.get_cells(), vec![]);
        assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));
        // No metadata map exists before sync — reads return None gracefully.
        assert_eq!(doc.get_metadata("runtime"), None);
        assert!(doc.get_metadata_snapshot().is_none());
    }

    #[test]
    fn test_empty_doc_set_metadata() {
        let mut doc = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        // set_metadata should create the metadata map on demand
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

    /// Regression test: MCP-added deps must survive when a second bootstrap
    /// peer (the app frontend) joins.
    ///
    /// Reproduces the exact production scenario:
    /// 1. Daemon creates notebook (new_with_actor → creates metadata map)
    /// 2. MCP agent bootstraps, syncs, then adds 10 deps
    /// 3. MCP syncs deps back to daemon
    /// 4. App frontend bootstraps and syncs — deps must survive
    ///
    /// Before the fix, bootstrap() created `metadata: {}` which competed
    /// with the daemon's metadata map via Automerge conflict resolution.
    /// If the bootstrap's empty map won, deps became invisible.
    #[test]
    fn test_bootstrap_sync_preserves_daemon_metadata() {
        use automerge::sync;

        fn sync_peers(
            a: &mut NotebookDoc,
            a_state: &mut sync::State,
            b: &mut NotebookDoc,
            b_state: &mut sync::State,
        ) {
            for _ in 0..10 {
                let msg_a = a.generate_sync_message(a_state);
                let msg_b = b.generate_sync_message(b_state);
                if msg_a.is_none() && msg_b.is_none() {
                    break;
                }
                if let Some(m) = msg_a {
                    b.receive_sync_message(b_state, m).unwrap();
                }
                if let Some(m) = msg_b {
                    a.receive_sync_message(a_state, m).unwrap();
                }
            }
        }

        // Step 1: Daemon creates notebook
        let mut daemon = NotebookDoc::new_with_actor("test-notebook", "runtimed");

        // Step 2: MCP agent bootstraps and syncs with daemon
        let mut mcp = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "mcp:nteract-nightly");
        let mut daemon_mcp_state = sync::State::new();
        let mut mcp_state = sync::State::new();
        sync_peers(&mut daemon, &mut daemon_mcp_state, &mut mcp, &mut mcp_state);

        // Step 3: MCP adds deps (writes to its local doc, syncs to daemon)
        for dep in &[
            "matplotlib",
            "pandas",
            "numpy",
            "pillow",
            "ipywidgets",
            "plotly",
            "altair",
            "vega_datasets",
            "sympy",
            "rich",
        ] {
            mcp.add_uv_dependency(dep).unwrap();
        }
        sync_peers(&mut daemon, &mut daemon_mcp_state, &mut mcp, &mut mcp_state);

        // Verify daemon has deps after MCP sync
        let daemon_snap = daemon.get_metadata_snapshot().unwrap();
        assert_eq!(
            daemon_snap.runt.uv.as_ref().unwrap().dependencies.len(),
            10,
            "Daemon must have 10 deps after MCP sync"
        );

        // Step 4: App frontend bootstraps and syncs — the critical test
        let mut frontend =
            NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:kyle:tab-1");
        let mut daemon_fe_state = sync::State::new();
        let mut fe_state = sync::State::new();
        sync_peers(
            &mut daemon,
            &mut daemon_fe_state,
            &mut frontend,
            &mut fe_state,
        );

        // Frontend must see all 10 deps
        let fe_snap = frontend.get_metadata_snapshot().unwrap();
        assert_eq!(
            fe_snap.runt.uv.as_ref().unwrap().dependencies.len(),
            10,
            "Frontend must see all 10 deps after bootstrap sync"
        );

        // Daemon must still have all 10 deps (not corrupted by frontend sync)
        let daemon_snap2 = daemon.get_metadata_snapshot().unwrap();
        assert_eq!(
            daemon_snap2.runt.uv.as_ref().unwrap().dependencies.len(),
            10,
            "Daemon deps must survive frontend bootstrap sync"
        );
    }

    /// Verify that old-style bootstrap creates metadata conflicts but
    /// the fixed bootstrap does not.
    #[test]
    fn test_old_bootstrap_creates_metadata_conflict_fixed_does_not() {
        use automerge::sync;

        fn sync_raw(
            a: &mut AutoCommit,
            a_state: &mut sync::State,
            b: &mut AutoCommit,
            b_state: &mut sync::State,
        ) {
            for _ in 0..10 {
                let msg_a = a.sync().generate_sync_message(a_state);
                let msg_b = b.sync().generate_sync_message(b_state);
                if msg_a.is_none() && msg_b.is_none() {
                    break;
                }
                if let Some(m) = msg_a {
                    b.sync().receive_sync_message(b_state, m).unwrap();
                }
                if let Some(m) = msg_b {
                    a.sync().receive_sync_message(a_state, m).unwrap();
                }
            }
        }

        // --- Part 1: old bootstrap creates a conflict ---
        let mut daemon1 = NotebookDoc::new_with_actor("test", "runtimed");
        daemon1.add_uv_dependency("numpy").unwrap();

        let mut old_bootstrap = AutoCommit::new();
        old_bootstrap.set_actor(ActorId::from(b"old-style-peer"));
        let _ = old_bootstrap.put(automerge::ROOT, "schema_version", SCHEMA_VERSION);
        let _ = old_bootstrap.put_object(automerge::ROOT, "cells", ObjType::Map);
        let _ = old_bootstrap.put_object(automerge::ROOT, "metadata", ObjType::Map);

        let mut ds1 = sync::State::new();
        let mut bs1 = sync::State::new();
        sync_raw(daemon1.doc_mut(), &mut ds1, &mut old_bootstrap, &mut bs1);

        let old_conflicts: Vec<_> = old_bootstrap
            .get_all(automerge::ROOT, "metadata")
            .unwrap_or_default();
        assert!(
            old_conflicts.len() >= 2,
            "Old bootstrap must create metadata conflict, got {} values",
            old_conflicts.len()
        );

        // --- Part 2: fixed bootstrap does NOT create a conflict ---
        let mut daemon2 = NotebookDoc::new_with_actor("test2", "runtimed");
        daemon2.add_uv_dependency("numpy").unwrap();

        let new_bootstrap = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "fixed-peer");
        let mut new_doc = new_bootstrap.into_inner();
        let mut ds2 = sync::State::new();
        let mut ns = sync::State::new();
        sync_raw(daemon2.doc_mut(), &mut ds2, &mut new_doc, &mut ns);

        let new_conflicts: Vec<_> = new_doc
            .get_all(automerge::ROOT, "metadata")
            .unwrap_or_default();
        assert_eq!(
            new_conflicts.len(),
            1,
            "Fixed bootstrap must not create metadata conflict"
        );

        // And deps are definitely visible
        let doc = NotebookDoc::wrap(new_doc);
        assert_eq!(
            doc.get_metadata_snapshot()
                .unwrap()
                .runt
                .uv
                .unwrap()
                .dependencies,
            vec!["numpy"]
        );
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
        doc.add_cell(1, "cell-2", "markdown").unwrap();
        doc.update_source("cell-2", "# Hello").unwrap();

        let bytes = doc.save();
        let loaded = NotebookDoc::load(&bytes).unwrap();

        assert_eq!(loaded.notebook_id(), Some("nb1".to_string()));
        let cells = loaded.get_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].source, "x = 42");
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
    #[cfg(feature = "persistence")]
    fn test_load_v3_doc_migrates_to_v4() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("notebook.automerge");

        // Create a doc with cells and deps, then downgrade to v3
        let mut doc = NotebookDoc::new("migrate-test");
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "import numpy").unwrap();
        doc.add_conda_dependency("numpy").unwrap();
        let _ = doc.doc.put(automerge::ROOT, "schema_version", 3u64);
        assert_eq!(doc.schema_version(), Some(3));
        doc.save_to_file(&path).unwrap();

        // load_or_create should migrate, not discard
        let loaded = NotebookDoc::load_or_create(&path, "migrate-test");
        assert_eq!(loaded.schema_version(), Some(SCHEMA_VERSION));
        assert_eq!(loaded.cell_count(), 1);
        let cells = loaded.get_cells();
        assert_eq!(cells[0].source, "import numpy");

        let snap = loaded.get_metadata_snapshot().unwrap();
        let conda = snap.runt.conda.unwrap();
        assert!(
            conda.dependencies.contains(&"numpy".to_string()),
            "conda deps must survive migration: {:?}",
            conda.dependencies
        );

        // Original file should NOT be renamed to .corrupt
        assert!(path.exists(), "migrated file should remain in place");
        let corrupt_path = path.with_extension("automerge.corrupt");
        assert!(
            !corrupt_path.exists(),
            "v3 migration should not create .corrupt"
        );
    }

    #[test]
    fn test_sync_between_two_docs() {
        // Server creates a notebook with cells
        let mut server = NotebookDoc::new("sync-test");
        server.add_cell(0, "cell-1", "code").unwrap();
        server.update_source("cell-1", "import numpy").unwrap();

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

    /// Tests legacy execution count fallback sync propagates correctly.
    #[test]
    fn test_legacy_execution_count_sync() {
        let mut daemon = NotebookDoc::new("exec-count-test");
        daemon
            .add_cell_full(
                "cell-1",
                "code",
                "80",
                "print('from file')",
                "7",
                &serde_json::json!({}),
            )
            .unwrap();

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

        // Live execution_count is covered by RuntimeStateDoc tests.
        // NotebookDoc syncs only the persisted nbformat-history fallback.
        assert_eq!(
            client.get_cell_execution_count("cell-1").as_deref(),
            Some("7")
        );
    }

    #[test]
    fn test_new_cells_default_to_null_legacy_execution_count() {
        let mut doc = NotebookDoc::new("exec-count-default-test");
        doc.add_cell(0, "cell-1", "code").unwrap();

        assert_eq!(
            doc.get_cell_execution_count("cell-1").as_deref(),
            Some("null")
        );
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

        // Daemon adds a cell
        daemon.add_cell(0, "daemon-cell", "code").unwrap();
        daemon.update_source("daemon-cell", "print(42)").unwrap();

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
            "null",
            &serde_json::json!({}),
        )
        .unwrap();

        let cell = doc.get_cell("cell-es").unwrap();
        assert_eq!(cell.source, "");
        assert_eq!(cell.execution_count, "null");
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
            "null",
            &serde_json::json!({}),
        )
        .unwrap();
        doc.add_cell_full(
            "b",
            "code",
            &pos_b.to_string(),
            "second",
            "null",
            &serde_json::json!({}),
        )
        .unwrap();
        doc.add_cell_full(
            "c",
            "code",
            &pos_c.to_string(),
            "third",
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

        doc.add_cell(1, "b", "code").unwrap();

        // Move a after b
        doc.move_cell("a", Some("b")).unwrap();

        // Verify content preserved
        let cell = doc.get_cell("a").unwrap();
        assert_eq!(cell.source, "original source");
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
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: Some(metadata::LanguageInfoSnapshot {
                name: "python".to_string(),
                version: Some("3.11.5".to_string()),
                extras: std::collections::BTreeMap::new(),
            }),
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
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
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        // kernelspec gets written. `runt` is empty (default) so the
        // snapshot writer intentionally skips it to avoid stamping
        // synthetic metadata on vanilla notebooks.
        let meta_id = doc.metadata_map_id().unwrap();
        assert!(doc.get_json_value(&meta_id, "kernelspec").is_some());
        assert!(
            doc.get_json_value(&meta_id, "runt").is_none(),
            "empty runt must not be written to the doc"
        );

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
        doc.add_cell_full("cell1", "code", "80", "x = 1", "null", &meta)
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
        doc.add_cell_full("cell1", "code", "80", "x = 1", "null", &meta)
            .unwrap();

        // Use the free function (as sync client would)
        let cells = get_cells_from_doc(doc.doc());
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].metadata, meta);
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

        // Verify metadata default to empty object
        assert_eq!(doc.get_cell_metadata("cell-a"), Some(serde_json::json!({})));

        // Verify position is present
        assert!(doc.get_cell_position("cell-a").is_some());
        assert!(doc.get_cell_position("cell-b").is_some());

        // Verify nonexistent cell returns None for all accessors
        assert_eq!(doc.get_cell_source("nonexistent"), None);
        assert_eq!(doc.get_cell_type("nonexistent"), None);
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
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
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
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
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
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
        };

        doc.set_metadata_snapshot(&snapshot).unwrap();

        let fp_before = doc.get_metadata_fingerprint().unwrap();

        doc.add_cell_after("cell-1", "code", None).unwrap();
        doc.update_source("cell-1", "print('hello')").unwrap();

        let fp_after = doc.get_metadata_fingerprint().unwrap();
        assert_eq!(fp_before, fp_after);
    }

    // ── update_json_at_key tests ──────────────────────────────────────

    #[test]
    fn test_update_json_preserves_existing_map_obj_id() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // Create initial map at "data"
        let map_id = doc.put_object(&root, "data", ObjType::Map).unwrap();
        doc.put(&map_id, "x", 1_i64).unwrap();

        // Record the ObjId
        let (_, original_id) = doc.get(&root, "data").unwrap().unwrap();

        // Update via update_json_at_key — should reuse the same Map object
        let new_val = serde_json::json!({"x": 2, "y": 3});
        update_json_at_key(&mut doc, &root, "data", &new_val).unwrap();

        let (_, updated_id) = doc.get(&root, "data").unwrap().unwrap();
        assert_eq!(original_id, updated_id, "Map ObjId should be preserved");

        // Verify contents updated
        let read_back = read_json_value(&doc, &root, "data");
        assert_eq!(read_back, Some(new_val));
    }

    #[test]
    fn test_update_json_removes_stale_map_keys() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let initial = serde_json::json!({"a": 1, "b": 2, "c": 3});
        update_json_at_key(&mut doc, &root, "m", &initial).unwrap();

        // Update with fewer keys — "b" should be removed
        let updated = serde_json::json!({"a": 10, "c": 30});
        update_json_at_key(&mut doc, &root, "m", &updated).unwrap();

        let read_back = read_json_value(&doc, &root, "m");
        assert_eq!(read_back, Some(updated));
    }

    #[test]
    fn test_update_json_resizes_list_grow() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let short = serde_json::json!([1, 2]);
        update_json_at_key(&mut doc, &root, "arr", &short).unwrap();

        let longer = serde_json::json!([1, 2, 3, 4]);
        update_json_at_key(&mut doc, &root, "arr", &longer).unwrap();

        let read_back = read_json_value(&doc, &root, "arr");
        assert_eq!(read_back, Some(longer));
    }

    #[test]
    fn test_update_json_resizes_list_shrink() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let long = serde_json::json!([1, 2, 3, 4, 5]);
        update_json_at_key(&mut doc, &root, "arr", &long).unwrap();

        let shorter = serde_json::json!([10, 20]);
        update_json_at_key(&mut doc, &root, "arr", &shorter).unwrap();

        let read_back = read_json_value(&doc, &root, "arr");
        assert_eq!(read_back, Some(shorter));
    }

    #[test]
    fn test_update_json_handles_type_change_scalar_to_object() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // Start with a scalar
        doc.put(&root, "val", "hello").unwrap();

        // Update to an object
        let obj = serde_json::json!({"nested": true});
        update_json_at_key(&mut doc, &root, "val", &obj).unwrap();

        let read_back = read_json_value(&doc, &root, "val");
        assert_eq!(read_back, Some(obj));
    }

    #[test]
    fn test_update_json_handles_type_change_object_to_scalar() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // Start with an object
        let obj = serde_json::json!({"nested": true});
        update_json_at_key(&mut doc, &root, "val", &obj).unwrap();

        // Replace with a scalar
        let scalar = serde_json::json!("just a string");
        update_json_at_key(&mut doc, &root, "val", &scalar).unwrap();

        let read_back = read_json_value(&doc, &root, "val");
        assert_eq!(read_back, Some(scalar));
    }

    #[test]
    fn test_update_json_preserves_existing_list_obj_id() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let arr = serde_json::json!([1, 2, 3]);
        update_json_at_key(&mut doc, &root, "list", &arr).unwrap();
        let (_, original_id) = doc.get(&root, "list").unwrap().unwrap();

        let arr2 = serde_json::json!([10, 20]);
        update_json_at_key(&mut doc, &root, "list", &arr2).unwrap();
        let (_, updated_id) = doc.get(&root, "list").unwrap().unwrap();

        assert_eq!(original_id, updated_id, "List ObjId should be preserved");
        let read_back = read_json_value(&doc, &root, "list");
        assert_eq!(read_back, Some(arr2));
    }

    #[test]
    fn test_update_json_nested_objects() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        let v1 = serde_json::json!({"uv": {"dependencies": ["numpy", "pandas"]}});
        update_json_at_key(&mut doc, &root, "runt", &v1).unwrap();

        // Update nested — should reuse outer "runt" and inner "uv" maps
        let v2 = serde_json::json!({"uv": {"dependencies": ["numpy", "scipy"]}, "pixi": {}});
        update_json_at_key(&mut doc, &root, "runt", &v2).unwrap();

        let read_back = read_json_value(&doc, &root, "runt");
        assert_eq!(read_back, Some(v2));
    }

    #[test]
    fn test_update_json_list_element_type_change() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // List with mixed types
        let v1 = serde_json::json!([1, "two", {"three": 3}]);
        update_json_at_key(&mut doc, &root, "mixed", &v1).unwrap();

        // Change element types
        let v2 = serde_json::json!([{"one": 1}, 2, "three"]);
        update_json_at_key(&mut doc, &root, "mixed", &v2).unwrap();

        let read_back = read_json_value(&doc, &root, "mixed");
        assert_eq!(read_back, Some(v2));
    }

    #[test]
    fn test_set_metadata_snapshot_with_update_json() {
        let mut doc = NotebookDoc::new("nb-update-meta");

        let snapshot1 = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: None,
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
        };
        doc.set_metadata_snapshot(&snapshot1).unwrap();

        // Update with different data
        let snapshot2 = metadata::NotebookMetadataSnapshot {
            kernelspec: Some(metadata::KernelspecSnapshot {
                name: "deno".to_string(),
                display_name: "Deno".to_string(),
                language: Some("typescript".to_string()),
                extras: std::collections::BTreeMap::new(),
            }),
            language_info: Some(metadata::LanguageInfoSnapshot {
                name: "typescript".to_string(),
                version: None,
                extras: std::collections::BTreeMap::new(),
            }),
            runt: metadata::RuntMetadata::default(),
            extras: std::collections::BTreeMap::new(),
        };
        doc.set_metadata_snapshot(&snapshot2).unwrap();

        let read_back = doc.get_metadata_snapshot().unwrap();
        assert_eq!(read_back.kernelspec.as_ref().unwrap().name, "deno");
        assert_eq!(read_back.language_info.as_ref().unwrap().name, "typescript");
    }

    #[test]
    fn set_metadata_snapshot_writes_extras_as_siblings() {
        use crate::metadata::NotebookMetadataSnapshot;
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");
        let mut snap = NotebookMetadataSnapshot::default();
        snap.extras.insert(
            "jupytext".to_string(),
            serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
        );
        snap.extras.insert(
            "colab".to_string(),
            serde_json::json!({"kernel": {"name": "python3"}}),
        );
        doc.set_metadata_snapshot(&snap).unwrap();

        let round_tripped = doc.get_metadata_snapshot().unwrap();
        assert_eq!(round_tripped.extras.len(), 2);
        assert_eq!(
            round_tripped.extras.get("jupytext"),
            Some(&serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}))
        );
        assert_eq!(
            round_tripped.extras.get("colab"),
            Some(&serde_json::json!({"kernel": {"name": "python3"}}))
        );
    }

    #[test]
    fn set_metadata_snapshot_drops_extras_colliding_with_kernelspec() {
        use crate::metadata::{KernelspecSnapshot, NotebookMetadataSnapshot};
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

        let mut snap = NotebookMetadataSnapshot {
            kernelspec: Some(KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
                extras: Default::default(),
            }),
            ..Default::default()
        };
        snap.extras
            .insert("kernelspec".to_string(), serde_json::json!({"BAD": true}));

        doc.set_metadata_snapshot(&snap).unwrap();

        let round_tripped = doc.get_metadata_snapshot().unwrap();
        assert_eq!(
            round_tripped.kernelspec.as_ref().unwrap().name,
            "python3",
            "typed kernelspec must survive collision"
        );
        assert!(
            !round_tripped.extras.contains_key("kernelspec"),
            "collision-dropped key must not appear in extras"
        );
    }

    #[test]
    fn set_metadata_snapshot_drops_extras_colliding_with_language_info() {
        use crate::metadata::{LanguageInfoSnapshot, NotebookMetadataSnapshot};
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

        let mut snap = NotebookMetadataSnapshot {
            language_info: Some(LanguageInfoSnapshot {
                name: "python".to_string(),
                version: Some("3.11.5".to_string()),
                extras: Default::default(),
            }),
            ..Default::default()
        };
        snap.extras.insert(
            "language_info".to_string(),
            serde_json::json!({"BAD": true}),
        );

        doc.set_metadata_snapshot(&snap).unwrap();

        let round_tripped = doc.get_metadata_snapshot().unwrap();
        assert_eq!(round_tripped.language_info.as_ref().unwrap().name, "python");
        assert!(!round_tripped.extras.contains_key("language_info"));
    }

    #[test]
    fn set_metadata_snapshot_drops_extras_colliding_with_runt() {
        use crate::metadata::{NotebookMetadataSnapshot, RuntMetadata};
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

        let mut snap = NotebookMetadataSnapshot {
            runt: RuntMetadata {
                env_id: Some("real-env-id".to_string()),
                ..RuntMetadata::default()
            },
            ..Default::default()
        };
        snap.extras
            .insert("runt".to_string(), serde_json::json!({"env_id": "bogus"}));

        doc.set_metadata_snapshot(&snap).unwrap();

        let round_tripped = doc.get_metadata_snapshot().unwrap();
        assert_eq!(
            round_tripped.runt.env_id.as_deref(),
            Some("real-env-id"),
            "typed runt must win over colliding extras"
        );
        assert!(!round_tripped.extras.contains_key("runt"));
    }

    #[test]
    fn set_metadata_snapshot_deletes_stale_extras_absent_from_replacement() {
        use crate::metadata::NotebookMetadataSnapshot;
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

        // Initial write: two unknown keys.
        let mut first = NotebookMetadataSnapshot::default();
        first
            .extras
            .insert("jupytext".to_string(), serde_json::json!({"formats": "py"}));
        first.extras.insert(
            "colab".to_string(),
            serde_json::json!({"kernel": "python3"}),
        );
        doc.set_metadata_snapshot(&first).unwrap();

        let after_first = doc.get_metadata_snapshot().unwrap();
        assert_eq!(after_first.extras.len(), 2);

        // Replacement: only jupytext present. colab must be deleted,
        // not quietly retained. This is the file-watcher-reload path —
        // a user edits metadata on disk, we re-parse, and the daemon
        // has to converge to the new shape.
        let mut second = NotebookMetadataSnapshot::default();
        second
            .extras
            .insert("jupytext".to_string(), serde_json::json!({"formats": "py"}));
        doc.set_metadata_snapshot(&second).unwrap();

        let after_second = doc.get_metadata_snapshot().unwrap();
        assert!(after_second.extras.contains_key("jupytext"));
        assert!(
            !after_second.extras.contains_key("colab"),
            "stale extras key must be deleted when absent from replacement"
        );
    }

    #[test]
    fn set_metadata_snapshot_preserves_runtime_and_ephemeral_scalars() {
        use crate::metadata::NotebookMetadataSnapshot;
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

        // Seed nteract-internal scalars directly on the metadata map —
        // bootstrap writes `runtime` and rooms may carry `ephemeral`.
        let meta_id = doc
            .doc
            .get(automerge::ROOT, "metadata")
            .unwrap()
            .and_then(|(v, id)| match v {
                automerge::Value::Object(automerge::ObjType::Map) => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| {
                doc.doc
                    .put_object(automerge::ROOT, "metadata", automerge::ObjType::Map)
                    .unwrap()
            });
        doc.doc.put(&meta_id, "runtime", "python").unwrap();
        doc.doc.put(&meta_id, "ephemeral", true).unwrap();

        // Add an extras key, then replace with a snapshot that has NO
        // extras. The runtime/ephemeral scalars must survive the
        // stale-extras sweep — they're not nbformat keys and must not
        // bleed into extras scans either.
        let mut first = NotebookMetadataSnapshot::default();
        first
            .extras
            .insert("jupytext".to_string(), serde_json::json!({"formats": "py"}));
        doc.set_metadata_snapshot(&first).unwrap();

        let empty = NotebookMetadataSnapshot::default();
        doc.set_metadata_snapshot(&empty).unwrap();

        // runtime and ephemeral scalars still on the doc.
        let runtime_scalar = read_str(&doc.doc, meta_id.clone(), "runtime");
        assert_eq!(runtime_scalar.as_deref(), Some("python"));
        let ephemeral_present = doc.doc.get(&meta_id, "ephemeral").unwrap().is_some();
        assert!(ephemeral_present);

        // jupytext was deleted.
        let after = doc.get_metadata_snapshot();
        assert!(after.is_none() || !after.unwrap().extras.contains_key("jupytext"));
    }

    #[test]
    fn get_metadata_snapshot_from_doc_reads_extras() {
        use crate::metadata::NotebookMetadataSnapshot;
        let mut doc = NotebookDoc::new_with_actor("test-nb", "test");
        let mut snap = NotebookMetadataSnapshot::default();
        snap.extras.insert(
            "jupytext".to_string(),
            serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
        );
        doc.set_metadata_snapshot(&snap).unwrap();

        // Read via the free-function path (used by notebook-sync +
        // Python bindings, not the &self method).
        let round_tripped = crate::get_metadata_snapshot_from_doc(doc.doc())
            .expect("free function should surface extras");
        assert!(round_tripped.extras.contains_key("jupytext"));
    }
}
