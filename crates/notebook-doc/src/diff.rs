//! Cell-level change detection via Automerge structural diffs.
//!
//! The core primitive: given two sets of Automerge heads (before/after),
//! determine which cells changed and which fields within each cell were
//! modified. This avoids full-notebook materialization on every sync
//! frame — consumers only re-read the cells and fields that actually changed.
//!
//! The implementation walks `doc.diff(before, after)` patches and extracts
//! cell IDs and field names from the patch paths. Cost is proportional to
//! the delta, not the document size.
//!
//! Used by:
//! - `runtimed-wasm`: replaces source-only `compute_text_attributions` with
//!   full field-level change detection in `receive_frame`
//! - `runtimed-py`: per-cell accessors + changeset for efficient MCP reads
//! - daemon: targeted persistence and output dispatch

use std::collections::{BTreeMap, BTreeSet};

use automerge::{patches::PatchAction, AutoCommit, ChangeHash, Prop};

/// What changed between two sets of Automerge heads.
///
/// Returned by [`diff_cells`]. The consumer uses this to decide which cells
/// need re-reading and which fields within those cells to materialize.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellChangeset {
    /// Cells that existed before and after, with field-level change info.
    /// Keyed by cell ID, sorted for deterministic iteration.
    pub changed: Vec<ChangedCell>,

    /// Cell IDs that were added (new keys in the cells map).
    pub added: Vec<String>,

    /// Cell IDs that were removed from the cells map.
    pub removed: Vec<String>,

    /// Whether cell ordering changed (any position field was modified,
    /// or cells were added/removed).
    pub order_changed: bool,
}

impl CellChangeset {
    /// Returns true if nothing changed at all.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// Returns true if only source fields changed (common case: typing).
    /// Useful for the frontend to decide between `updateCellById` (cheap)
    /// and full materialization (expensive).
    pub fn is_source_only(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && !self.order_changed
            && self.changed.iter().all(|c| c.fields.is_source_only())
    }

    /// All cell IDs that need any kind of re-read (changed + added).
    pub fn affected_cell_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.changed.iter().map(|c| c.cell_id.as_str()).collect();
        ids.extend(self.added.iter().map(|s| s.as_str()));
        ids
    }

    /// Whether this changeset represents a structural change (cells added,
    /// removed, or reordered) that requires updating the cell ID list.
    pub fn is_structural(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || self.order_changed
    }
}

/// A single cell that changed, with field-level granularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedCell {
    pub cell_id: String,
    pub fields: ChangedFields,
}

/// Bitflags-style struct for which cell fields changed.
///
/// Using individual bools instead of a bitflags crate — the cell schema is
/// stable and this avoids a dependency. Crosses the WASM boundary as a
/// plain object (no Vec<String> allocation).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangedFields {
    pub source: bool,
    pub outputs: bool,
    pub execution_count: bool,
    pub cell_type: bool,
    pub metadata: bool,
    pub position: bool,
    pub resolved_assets: bool,
}

impl ChangedFields {
    /// Returns true if only the source field changed.
    pub fn is_source_only(&self) -> bool {
        self.source
            && !self.outputs
            && !self.execution_count
            && !self.cell_type
            && !self.metadata
            && !self.position
            && !self.resolved_assets
    }

    /// Returns true if no fields are marked as changed.
    pub fn is_empty(&self) -> bool {
        !self.source
            && !self.outputs
            && !self.execution_count
            && !self.cell_type
            && !self.metadata
            && !self.position
            && !self.resolved_assets
    }

    /// Set the flag for a named field. Returns true if the field name was recognized.
    fn set_field(&mut self, name: &str) -> bool {
        match name {
            "source" => self.source = true,
            "outputs" => self.outputs = true,
            "execution_count" => self.execution_count = true,
            "cell_type" => self.cell_type = true,
            "metadata" => self.metadata = true,
            "position" => self.position = true,
            "resolved_assets" => self.resolved_assets = true,
            // execution_id change means the cell points to a different execution's
            // outputs in RuntimeStateDoc — trigger output re-materialization
            "execution_id" => self.outputs = true,
            "id" => { /* Cell ID field — not a meaningful change */ }
            _ => return false,
        }
        true
    }

    /// Merge another ChangedFields into this one (union of flags).
    fn merge(&mut self, other: &ChangedFields) {
        self.source |= other.source;
        self.outputs |= other.outputs;
        self.execution_count |= other.execution_count;
        self.cell_type |= other.cell_type;
        self.metadata |= other.metadata;
        self.position |= other.position;
        self.resolved_assets |= other.resolved_assets;
    }
}

/// A splice or deletion on a cell's source text, extracted from an
/// Automerge diff.
///
/// Accompanies [`DocChangeset::text_patches`] so consumers can build text
/// attribution ranges without re-walking the patches themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPatch {
    /// The cell whose source was modified.
    pub cell_id: String,
    /// Character index where the splice/delete occurred.
    pub index: usize,
    /// Text that was inserted (empty for pure deletions).
    pub text: String,
    /// Number of characters deleted at this index (0 for pure inserts).
    pub deleted: usize,
}

/// Full structural classification of a diff between two sets of Automerge
/// heads.
///
/// Produced by [`diff_doc`]. One `doc.diff()` call, one pass over the
/// patches, every downstream consumer satisfied:
///
/// - WASM's `receive_frame` reads `cells` + `text_patches` (combined with
///   a separate `extract_change_actors` call to tag ownership).
/// - The daemon reads `metadata_changed` to decide whether to broadcast
///   env-sync state.
/// - Future patch categories (per-cell execution state, trust, etc.) can
///   extend this struct without any extra `doc.diff()` calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocChangeset {
    /// Cell-level structural changeset — what cells changed and which
    /// fields within them.
    pub cells: CellChangeset,

    /// True if any patch touched the top-level `metadata` map (notebook
    /// metadata, not per-cell metadata).
    pub metadata_changed: bool,

    /// Raw source-text splice/delete events, one per Automerge
    /// SpliceText or DeleteSeq on a cell's source. Pair with actors
    /// from [`extract_change_actors`] to build `TextAttribution`s.
    pub text_patches: Vec<TextPatch>,
}

impl DocChangeset {
    /// Returns true if nothing of interest changed.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && !self.metadata_changed && self.text_patches.is_empty()
    }
}

/// Single-pass diff: walk `doc.diff(before, after)` once and classify
/// every patch into cells / metadata / text-patch buckets.
///
/// Replaces the pattern where consumers called `diff_cells`,
/// `diff_metadata_touched`, and `compute_text_attributions` in sequence
/// and each re-ran `doc.diff` on the same heads. Cost goes from O(N*3)
/// to O(N) where N is the number of patches.
///
/// # Arguments
///
/// Same semantics as [`diff_cells`] — `before == &[]` or `before == after`
/// returns an empty changeset.
pub fn diff_doc(doc: &mut AutoCommit, before: &[ChangeHash], after: &[ChangeHash]) -> DocChangeset {
    // No previous state — caller should do full materialization.
    if before.is_empty() {
        return DocChangeset::default();
    }

    // Nothing changed.
    if before == after {
        return DocChangeset::default();
    }

    let patches = doc.diff(before, after);

    let mut cell_fields: BTreeMap<String, ChangedFields> = BTreeMap::new();
    let mut added: BTreeSet<String> = BTreeSet::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();
    let mut order_changed = false;
    let mut metadata_changed = false;
    let mut text_patches: Vec<TextPatch> = Vec::new();

    for patch in &patches {
        // 1. Top-level metadata — either the path starts with "metadata"
        //    (we're inside the metadata subtree) or the action is a
        //    put/delete on the "metadata" key at ROOT (no path).
        let starts_with_metadata = patch
            .path
            .first()
            .is_some_and(|(_, prop)| matches!(prop, Prop::Map(k) if k == "metadata"));
        if starts_with_metadata {
            metadata_changed = true;
        } else if patch.path.is_empty() {
            match &patch.action {
                PatchAction::PutMap { key, .. } if key == "metadata" => {
                    metadata_changed = true;
                }
                PatchAction::DeleteMap { key } if key == "metadata" => {
                    metadata_changed = true;
                }
                _ => {}
            }
        }

        // 2. Cells subtree — extract cell field changes and collect
        //    source-text splice/delete actions as TextPatches.
        let cells_idx = patch
            .path
            .iter()
            .position(|(_, prop)| matches!(prop, Prop::Map(k) if k == "cells"));

        let Some(cells_idx) = cells_idx else {
            continue;
        };

        let path_after_cells = &patch.path[(cells_idx + 1)..];

        match path_after_cells.len() {
            0 => {
                // Path ends at "cells" — the action is on the cells map itself.
                match &patch.action {
                    PatchAction::PutMap { key, .. } => {
                        added.insert(key.clone());
                        order_changed = true;
                    }
                    PatchAction::DeleteMap { key } => {
                        removed.insert(key.clone());
                        order_changed = true;
                    }
                    _ => {}
                }
            }
            _ => {
                let cell_id = match &path_after_cells[0].1 {
                    Prop::Map(id) => id.clone(),
                    _ => continue,
                };

                let is_added = added.contains(&cell_id);

                // Source-text splice/delete always produces a TextPatch,
                // even for cells that were added in this same diff
                // window — the frontend wants attribution overlays on
                // every freshly-arrived character, regardless of
                // whether the cell itself is new. Matches the pre-
                // refactor `compute_text_attributions` contract.
                if path_after_cells.len() >= 2 {
                    if let Prop::Map(f) = &path_after_cells[1].1 {
                        if f == "source" {
                            match &patch.action {
                                PatchAction::SpliceText { index, value, .. } => {
                                    let text = value.make_string();
                                    if !text.is_empty() {
                                        text_patches.push(TextPatch {
                                            cell_id: cell_id.clone(),
                                            index: *index,
                                            text,
                                            deleted: 0,
                                        });
                                    }
                                }
                                PatchAction::DeleteSeq { index, length } => {
                                    text_patches.push(TextPatch {
                                        cell_id: cell_id.clone(),
                                        index: *index,
                                        text: String::new(),
                                        deleted: *length,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // Field-level change tracking. Skip when the cell was
                // just added in this same diff — the caller will read
                // the whole cell fresh from the doc anyway, so there's
                // no benefit to ALSO marking individual fields as
                // changed. This also matches the pre-refactor
                // `diff_cells` behavior.
                if is_added {
                    continue;
                }

                if path_after_cells.len() == 1 {
                    match &patch.action {
                        PatchAction::PutMap { key, .. } => {
                            let fields = cell_fields.entry(cell_id).or_default();
                            fields.set_field(key);
                            if key == "position" {
                                order_changed = true;
                            }
                        }
                        PatchAction::DeleteMap { key } => {
                            let fields = cell_fields.entry(cell_id).or_default();
                            fields.set_field(key);
                        }
                        _ => {}
                    }
                } else {
                    let field_name = match &path_after_cells[1].1 {
                        Prop::Map(f) => f.as_str(),
                        _ => continue,
                    };

                    let fields = cell_fields.entry(cell_id).or_default();
                    fields.set_field(field_name);

                    if field_name == "position" {
                        order_changed = true;
                    }
                }
            }
        }
    }

    for id in &added {
        cell_fields.remove(id);
    }
    for id in &removed {
        cell_fields.remove(id);
    }

    DocChangeset {
        cells: CellChangeset {
            changed: cell_fields
                .into_iter()
                .filter(|(_, fields)| !fields.is_empty())
                .map(|(cell_id, fields)| ChangedCell { cell_id, fields })
                .collect(),
            added: added.into_iter().collect(),
            removed: removed.into_iter().collect(),
            order_changed,
        },
        metadata_changed,
        text_patches,
    }
}

/// Compute which cells changed between two sets of Automerge heads.
///
/// Walks `doc.diff(before, after)` patches and extracts cell IDs and field
/// names from the patch paths. Cost is proportional to the number of
/// operations in the delta, not the document size.
///
/// # Arguments
///
/// * `doc` — The Automerge document (`&mut` because `AutoCommit::diff` may
///   close a pending transaction).
/// * `before` — Heads before the change (e.g., captured before `receive_sync_message`).
///   Pass `&[]` for initial sync (returns empty changeset — caller should do full materialization).
/// * `after` — Heads after the change.
///
/// # Returns
///
/// A [`CellChangeset`] describing which cells changed and how. Returns
/// `CellChangeset::default()` (empty) if heads are equal or `before` is empty.
pub fn diff_cells(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
) -> CellChangeset {
    diff_doc(doc, before, after).cells
}

/// Extract actor labels from the changes between two head sets.
///
/// This is a companion to [`diff_cells`] — it tells you WHO made the changes
/// (e.g., `"runtimed"`, `"human:abc"`, `"agent:claude:xy"`), while `diff_cells`
/// tells you WHAT changed.
///
/// Returns a sorted, deduplicated list of actor labels.
pub fn extract_change_actors(doc: &mut AutoCommit, before: &[ChangeHash]) -> Vec<String> {
    let new_changes = doc.get_changes(before);
    let mut actors: BTreeSet<String> = BTreeSet::new();
    for change in &new_changes {
        actors.insert(crate::actor_label_from_id(change.actor_id()));
    }
    actors.into_iter().collect()
}

/// Merge two changesets (e.g., when coalescing multiple sync frames).
///
/// Used by the frontend's `scheduleMaterialize` to accumulate changes across
/// the 32ms coalescing window before applying them.
pub fn merge_changesets(a: &CellChangeset, b: &CellChangeset) -> CellChangeset {
    let mut cell_fields: BTreeMap<String, ChangedFields> = BTreeMap::new();

    // Merge changed cells from both.
    for changed in a.changed.iter().chain(b.changed.iter()) {
        cell_fields
            .entry(changed.cell_id.clone())
            .or_default()
            .merge(&changed.fields);
    }

    // Merge added/removed sets.
    let mut added: BTreeSet<String> = BTreeSet::new();
    added.extend(a.added.iter().cloned());
    added.extend(b.added.iter().cloned());

    let mut removed: BTreeSet<String> = BTreeSet::new();
    removed.extend(a.removed.iter().cloned());
    removed.extend(b.removed.iter().cloned());

    // A cell added in `a` then removed in `b` nets out to removed
    // (and vice versa). Handle the intersection.
    let added_then_removed: BTreeSet<String> = added.intersection(&removed).cloned().collect();
    for id in &added_then_removed {
        // If it was added in `a` and removed in `b`, it's a net removal only
        // if it wasn't in the original doc. We can't know that here without
        // the doc, so we conservatively keep it in `removed` and drop from `added`.
        // The caller will handle "remove nonexistent" gracefully.
        added.remove(id);
    }

    // Remove added/removed from changed.
    for id in added.iter().chain(removed.iter()) {
        cell_fields.remove(id);
    }

    CellChangeset {
        changed: cell_fields
            .into_iter()
            .filter(|(_, fields)| !fields.is_empty())
            .map(|(cell_id, fields)| ChangedCell { cell_id, fields })
            .collect(),
        added: added.into_iter().collect(),
        removed: removed.into_iter().collect(),
        order_changed: a.order_changed || b.order_changed,
    }
}

// ── Serde support ────────────────────────────────────────────────────
//
// These impls are needed for crossing the WASM boundary via serde-wasm-bindgen
// and for the Python bindings via serde_json.

impl serde::Serialize for ChangedFields {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Only serialize fields that are true — keeps the payload small.
        let field_count = [
            self.source,
            self.outputs,
            self.execution_count,
            self.cell_type,
            self.metadata,
            self.position,
            self.resolved_assets,
        ]
        .iter()
        .filter(|&&v| v)
        .count();

        let mut s = serializer.serialize_struct("ChangedFields", field_count)?;
        if self.source {
            s.serialize_field("source", &true)?;
        }
        if self.outputs {
            s.serialize_field("outputs", &true)?;
        }
        if self.execution_count {
            s.serialize_field("execution_count", &true)?;
        }
        if self.cell_type {
            s.serialize_field("cell_type", &true)?;
        }
        if self.metadata {
            s.serialize_field("metadata", &true)?;
        }
        if self.position {
            s.serialize_field("position", &true)?;
        }
        if self.resolved_assets {
            s.serialize_field("resolved_assets", &true)?;
        }
        s.end()
    }
}

impl<'de> serde::Deserialize<'de> for ChangedFields {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Helper {
            source: bool,
            outputs: bool,
            execution_count: bool,
            cell_type: bool,
            metadata: bool,
            position: bool,
            resolved_assets: bool,
        }

        let h = Helper::deserialize(deserializer)?;
        Ok(ChangedFields {
            source: h.source,
            outputs: h.outputs,
            execution_count: h.execution_count,
            cell_type: h.cell_type,
            metadata: h.metadata,
            position: h.position,
            resolved_assets: h.resolved_assets,
        })
    }
}

impl serde::Serialize for ChangedCell {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ChangedCell", 2)?;
        s.serialize_field("cell_id", &self.cell_id)?;
        s.serialize_field("fields", &self.fields)?;
        s.end()
    }
}

impl<'de> serde::Deserialize<'de> for ChangedCell {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Helper {
            cell_id: String,
            fields: ChangedFields,
        }
        let h = Helper::deserialize(deserializer)?;
        Ok(ChangedCell {
            cell_id: h.cell_id,
            fields: h.fields,
        })
    }
}

impl serde::Serialize for CellChangeset {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("CellChangeset", 4)?;
        s.serialize_field("changed", &self.changed)?;
        s.serialize_field("added", &self.added)?;
        s.serialize_field("removed", &self.removed)?;
        s.serialize_field("order_changed", &self.order_changed)?;
        s.end()
    }
}

impl<'de> serde::Deserialize<'de> for CellChangeset {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Helper {
            changed: Vec<ChangedCell>,
            added: Vec<String>,
            removed: Vec<String>,
            order_changed: bool,
        }
        let h = Helper::deserialize(deserializer)?;
        Ok(CellChangeset {
            changed: h.changed,
            added: h.added,
            removed: h.removed,
            order_changed: h.order_changed,
        })
    }
}

/// Check whether any Automerge patch between two head sets touches the
/// top-level `metadata` map.
///
/// This is a cheap pre-filter so the daemon can skip expensive metadata
/// materialization for cell-only sync frames (e.g., keystroke edits).
///
/// Returns `false` for empty `before` (initial sync) or equal heads —
/// the same convention as [`diff_cells`].
pub fn diff_metadata_touched(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
) -> bool {
    diff_doc(doc, before, after).metadata_changed
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::{NotebookDoc, TextEncoding};
    use automerge::sync;
    use automerge::transaction::Transactable;

    /// Helper: create two docs (daemon + client), sync to convergence,
    /// return the client's heads after sync.
    fn sync_docs(daemon: &mut NotebookDoc, client: &mut NotebookDoc) {
        let mut daemon_state = sync::State::new();
        let mut client_state = sync::State::new();
        for _ in 0..10 {
            let msg_d = daemon.generate_sync_message(&mut daemon_state);
            let msg_c = client.generate_sync_message(&mut client_state);
            if msg_d.is_none() && msg_c.is_none() {
                break;
            }
            if let Some(m) = msg_d {
                client.receive_sync_message(&mut client_state, m).unwrap();
            }
            if let Some(m) = msg_c {
                daemon.receive_sync_message(&mut daemon_state, m).unwrap();
            }
        }
    }

    #[test]
    fn test_no_change_returns_empty() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let heads = doc.doc_mut().get_heads();
        let changeset = diff_cells(doc.doc_mut(), &heads, &heads);
        assert!(changeset.is_empty());
    }

    #[test]
    fn test_empty_before_returns_empty() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let heads = doc.doc_mut().get_heads();
        let changeset = diff_cells(doc.doc_mut(), &[], &heads);
        assert!(changeset.is_empty());
    }

    #[test]
    fn test_source_edit_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "print('hello')").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        assert_eq!(changeset.changed[0].cell_id, "cell-1");
        assert!(changeset.changed[0].fields.source);
        assert!(!changeset.changed[0].fields.outputs);
        assert!(changeset.is_source_only());
    }

    #[test]
    fn test_execution_count_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        // Write execution_count directly via raw Automerge put (simulating legacy peer)
        let cell_obj = doc.cell_obj_for("cell-1").unwrap();
        doc.doc_mut()
            .put(&cell_obj, "execution_count", "1")
            .unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        assert!(changeset.changed[0].fields.execution_count);
    }

    #[test]
    fn test_cell_added_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.add_cell(1, "cell-2", "markdown").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert!(changeset.added.contains(&"cell-2".to_string()));
        assert!(changeset.order_changed);
        assert!(changeset.is_structural());
        // cell-2 should NOT appear in `changed` — it's in `added`.
        assert!(!changeset.changed.iter().any(|c| c.cell_id == "cell-2"));
    }

    #[test]
    fn test_cell_deleted_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.add_cell(1, "cell-2", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.delete_cell("cell-2").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert!(changeset.removed.contains(&"cell-2".to_string()));
        assert!(changeset.order_changed);
    }

    #[test]
    fn test_move_cell_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.add_cell(1, "cell-2", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.move_cell("cell-2", None).unwrap(); // Move to beginning
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert!(changeset.order_changed);
        // cell-2's position changed
        let cell2 = changeset
            .changed
            .iter()
            .find(|c| c.cell_id == "cell-2")
            .expect("cell-2 should be in changed");
        assert!(cell2.fields.position);
    }

    #[test]
    fn test_multiple_fields_on_same_cell() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "x = 1").unwrap();
        let cell_obj = doc.cell_obj_for("cell-1").unwrap();
        doc.doc_mut()
            .put(&cell_obj, "execution_count", "1")
            .unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        let cell = &changeset.changed[0];
        assert!(cell.fields.source);
        assert!(cell.fields.execution_count);
        assert!(!cell.fields.metadata);
    }

    #[test]
    fn test_multiple_cells_changed() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-a", "code").unwrap();
        doc.add_cell(1, "cell-b", "code").unwrap();
        doc.add_cell(2, "cell-c", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-a", "a = 1").unwrap();
        doc.update_source("cell-c", "c = 3").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 2);

        let ids: Vec<&str> = changeset
            .changed
            .iter()
            .map(|c| c.cell_id.as_str())
            .collect();
        assert!(ids.contains(&"cell-a"));
        assert!(ids.contains(&"cell-c"));
        // cell-b should not appear
        assert!(!ids.contains(&"cell-b"));
    }

    // ── diff_doc (unified) coverage ──────────────────────────────────
    //
    // The thin wrappers `diff_cells` and `diff_metadata_touched` are
    // covered above. These tests exercise the fields that only appear
    // on `DocChangeset` — `text_patches` and `metadata_changed` — and
    // the combined single-walk behavior.

    #[test]
    fn test_diff_doc_source_edit_populates_text_patches() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "print('hello')").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_doc(doc.doc_mut(), &before, &after);
        assert!(changeset.cells.is_source_only());
        assert_eq!(changeset.text_patches.len(), 1);
        assert_eq!(changeset.text_patches[0].cell_id, "cell-1");
        assert_eq!(changeset.text_patches[0].index, 0);
        assert_eq!(changeset.text_patches[0].text, "print('hello')");
        assert_eq!(changeset.text_patches[0].deleted, 0);
        assert!(!changeset.metadata_changed);
    }

    #[test]
    fn test_diff_doc_source_delete_produces_delete_patch() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "abcdef").unwrap();
        let before = doc.doc_mut().get_heads();

        // Delete "cd" — splice out 2 characters at index 2.
        doc.splice_source("cell-1", 2, 2, "").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_doc(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.text_patches.len(), 1);
        let tp = &changeset.text_patches[0];
        assert_eq!(tp.cell_id, "cell-1");
        assert_eq!(tp.index, 2);
        assert_eq!(tp.text, "");
        assert_eq!(tp.deleted, 2);
    }

    #[test]
    fn test_diff_doc_metadata_only_edit_sets_flag_only() {
        // A notebook-metadata-only edit should set `metadata_changed`
        // without populating cells or text_patches.
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.add_uv_dependency("numpy").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_doc(doc.doc_mut(), &before, &after);
        assert!(changeset.metadata_changed);
        assert!(changeset.cells.is_empty());
        assert!(changeset.text_patches.is_empty());
    }

    #[test]
    fn test_diff_doc_combined_source_and_metadata_single_walk() {
        // Cell source edit + notebook metadata edit in one diff window
        // — both should surface from a single `diff_doc` call.
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "x = 1").unwrap();
        doc.add_uv_dependency("polars").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_doc(doc.doc_mut(), &before, &after);
        assert!(
            changeset.metadata_changed,
            "metadata edit should be captured"
        );
        assert_eq!(
            changeset.text_patches.len(),
            1,
            "source edit should be captured"
        );
        assert!(
            !changeset.cells.is_empty(),
            "cells.changed should record source edit"
        );
    }

    // Equivalence tests: the new `diff_doc` is the single source of
    // truth for the three old entry points. These tests assert that
    // `diff_cells(...) == diff_doc(...).cells` and `diff_metadata_touched(...) ==
    // diff_doc(...).metadata_changed` across the full classifier
    // matrix, so a future change to diff_doc that silently drops a
    // category fails loudly here.

    fn build_diverse_diff(doc: &mut NotebookDoc) -> (Vec<ChangeHash>, Vec<ChangeHash>) {
        doc.add_cell(0, "cell-a", "code").unwrap();
        doc.add_cell(1, "cell-b", "markdown").unwrap();
        let before = doc.doc_mut().get_heads();

        // Every classifier case in one window: source edit,
        // per-cell metadata, notebook-level metadata, removal,
        // execution_count, cell_type change.
        doc.update_source("cell-a", "print('ok')").unwrap();
        let _ = doc.set_cell_source_hidden("cell-a", true);
        doc.add_uv_dependency("numpy").unwrap();
        doc.delete_cell("cell-b").unwrap();
        let cell_a_obj = doc.cell_obj_for("cell-a").unwrap();
        doc.doc_mut()
            .put(&cell_a_obj, "execution_count", 5_i64)
            .unwrap();

        let after = doc.doc_mut().get_heads();
        (before, after)
    }

    #[test]
    fn test_diff_doc_cells_matches_diff_cells() {
        let mut doc = NotebookDoc::new("nb1");
        let (before, after) = build_diverse_diff(&mut doc);

        let unified = diff_doc(doc.doc_mut(), &before, &after).cells;
        let legacy = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(unified, legacy);
    }

    #[test]
    fn test_diff_doc_metadata_changed_matches_diff_metadata_touched() {
        // Case 1: notebook-metadata edit — both agree.
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-a", "code").unwrap();
        let before = doc.doc_mut().get_heads();
        doc.add_uv_dependency("polars").unwrap();
        let after = doc.doc_mut().get_heads();
        assert!(diff_doc(doc.doc_mut(), &before, &after).metadata_changed);
        assert!(diff_metadata_touched(doc.doc_mut(), &before, &after));

        // Case 2: cell-only edit — both say false.
        let mut doc2 = NotebookDoc::new("nb2");
        doc2.add_cell(0, "cell-a", "code").unwrap();
        let before = doc2.doc_mut().get_heads();
        doc2.update_source("cell-a", "x = 1").unwrap();
        let after = doc2.doc_mut().get_heads();
        assert!(!diff_doc(doc2.doc_mut(), &before, &after).metadata_changed);
        assert!(!diff_metadata_touched(doc2.doc_mut(), &before, &after));

        // Case 3: per-cell metadata — the nested "metadata" under
        // cells/<id>/metadata is NOT notebook metadata. Both agree.
        let mut doc3 = NotebookDoc::new("nb3");
        doc3.add_cell(0, "cell-a", "code").unwrap();
        let before = doc3.doc_mut().get_heads();
        let _ = doc3.set_cell_source_hidden("cell-a", true);
        let after = doc3.doc_mut().get_heads();
        assert!(!diff_doc(doc3.doc_mut(), &before, &after).metadata_changed);
        assert!(!diff_metadata_touched(doc3.doc_mut(), &before, &after));
    }

    #[test]
    fn test_diff_doc_added_cell_still_emits_text_patches() {
        // Add-then-edit in a single diff window: the cell is in
        // `added` AND there are SpliceText patches on its source.
        // The frontend still wants attribution overlays on the
        // freshly-arrived characters — matches the pre-refactor
        // `compute_text_attributions` contract, which had no
        // added-cell filter.
        //
        // Field-level tracking IS skipped for added cells (the
        // caller will do a full read), but text_patches are NOT.
        let mut doc = NotebookDoc::new("nb1");
        let before = doc.doc_mut().get_heads();

        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 1").unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_doc(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.cells.added, vec!["cell-1".to_string()]);
        // The cell is in `added`, not `changed` — field tracking
        // skipped (old diff_cells behavior).
        assert!(
            changeset.cells.changed.is_empty(),
            "added cell should not appear in cells.changed, got {:?}",
            changeset.cells.changed
        );
        // But text_patches ARE emitted so attribution still works.
        assert_eq!(changeset.text_patches.len(), 1);
        assert_eq!(changeset.text_patches[0].cell_id, "cell-1");
        assert_eq!(changeset.text_patches[0].text, "x = 1");
    }

    #[test]
    fn test_metadata_change_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        let _ = doc.set_cell_source_hidden("cell-1", true);
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        assert!(changeset.changed[0].fields.metadata);
    }

    #[test]
    fn test_cell_type_change_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        let _ = doc.set_cell_type("cell-1", "markdown");
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        assert!(changeset.changed[0].fields.cell_type);
    }

    #[test]
    fn test_sync_source_edit_detected() {
        let mut daemon = NotebookDoc::new("nb1");
        daemon.add_cell(0, "cell-1", "code").unwrap();

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        sync_docs(&mut daemon, &mut client);

        let before = client.doc_mut().get_heads();

        // Daemon edits source, then sync to client.
        daemon.update_source("cell-1", "import os").unwrap();
        sync_docs(&mut daemon, &mut client);

        let after = client.doc_mut().get_heads();
        let changeset = diff_cells(client.doc_mut(), &before, &after);

        assert_eq!(changeset.changed.len(), 1);
        assert_eq!(changeset.changed[0].cell_id, "cell-1");
        assert!(changeset.changed[0].fields.source);
        assert!(changeset.is_source_only());
    }

    #[test]
    fn test_sync_execution_count() {
        let mut daemon = NotebookDoc::new("nb1");
        daemon.add_cell(0, "cell-1", "code").unwrap();
        daemon.update_source("cell-1", "print('hello')").unwrap();

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        sync_docs(&mut daemon, &mut client);

        let before = client.doc_mut().get_heads();

        // Daemon writes execution count (via raw Automerge put).
        let cell_obj = daemon.cell_obj_for("cell-1").unwrap();
        daemon
            .doc_mut()
            .put(&cell_obj, "execution_count", "1")
            .unwrap();
        sync_docs(&mut daemon, &mut client);

        let after = client.doc_mut().get_heads();
        let changeset = diff_cells(client.doc_mut(), &before, &after);

        assert_eq!(changeset.changed.len(), 1);
        let cell = &changeset.changed[0];
        assert!(cell.fields.execution_count);
        assert!(!cell.fields.source);
    }

    #[test]
    fn test_sync_add_cell() {
        let mut daemon = NotebookDoc::new("nb1");
        daemon.add_cell(0, "cell-1", "code").unwrap();

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "test");
        sync_docs(&mut daemon, &mut client);

        let before = client.doc_mut().get_heads();

        daemon.add_cell(1, "cell-2", "markdown").unwrap();
        sync_docs(&mut daemon, &mut client);

        let after = client.doc_mut().get_heads();
        let changeset = diff_cells(client.doc_mut(), &before, &after);

        assert!(changeset.added.contains(&"cell-2".to_string()));
        assert!(changeset.is_structural());
    }

    #[test]
    fn test_is_source_only_with_mixed() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.add_cell(1, "cell-2", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "a").unwrap();
        let cell_obj = doc.cell_obj_for("cell-2").unwrap();
        doc.doc_mut()
            .put(&cell_obj, "execution_count", "1")
            .unwrap();
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert!(!changeset.is_source_only());
    }

    #[test]
    fn test_merge_changesets_disjoint() {
        let a = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-1".into(),
                fields: ChangedFields {
                    source: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };

        let b = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-2".into(),
                fields: ChangedFields {
                    outputs: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };

        let merged = merge_changesets(&a, &b);
        assert_eq!(merged.changed.len(), 2);
    }

    #[test]
    fn test_merge_changesets_overlapping() {
        let a = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-1".into(),
                fields: ChangedFields {
                    source: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };

        let b = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-1".into(),
                fields: ChangedFields {
                    outputs: true,
                    execution_count: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };

        let merged = merge_changesets(&a, &b);
        assert_eq!(merged.changed.len(), 1);
        let cell = &merged.changed[0];
        assert!(cell.fields.source);
        assert!(cell.fields.outputs);
        assert!(cell.fields.execution_count);
    }

    #[test]
    fn test_merge_changesets_add_then_remove() {
        let a = CellChangeset {
            added: vec!["cell-new".into()],
            order_changed: true,
            ..Default::default()
        };

        let b = CellChangeset {
            removed: vec!["cell-new".into()],
            order_changed: true,
            ..Default::default()
        };

        let merged = merge_changesets(&a, &b);
        // Added in a, removed in b — removed from added, kept in removed.
        assert!(!merged.added.contains(&"cell-new".into()));
        assert!(merged.removed.contains(&"cell-new".into()));
        assert!(merged.order_changed);
    }

    #[test]
    fn test_changeset_affected_cell_ids() {
        let cs = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-1".into(),
                fields: ChangedFields {
                    source: true,
                    ..Default::default()
                },
            }],
            added: vec!["cell-2".into()],
            ..Default::default()
        };

        let ids = cs.affected_cell_ids();
        assert!(ids.contains(&"cell-1"));
        assert!(ids.contains(&"cell-2"));
    }

    #[test]
    fn test_serde_roundtrip() {
        let cs = CellChangeset {
            changed: vec![ChangedCell {
                cell_id: "cell-1".into(),
                fields: ChangedFields {
                    source: true,
                    outputs: true,
                    ..Default::default()
                },
            }],
            added: vec!["cell-2".into()],
            removed: vec!["cell-3".into()],
            order_changed: true,
        };

        let json = serde_json::to_string(&cs).unwrap();
        let deserialized: CellChangeset = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, deserialized);
    }

    #[test]
    fn test_changed_fields_serde_only_true_fields() {
        let fields = ChangedFields {
            source: true,
            outputs: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&fields).unwrap();
        // Only true fields should be present.
        assert!(json.contains("\"source\":true"));
        assert!(json.contains("\"outputs\":true"));
        assert!(!json.contains("metadata"));
        assert!(!json.contains("position"));
    }

    #[test]
    fn test_resolved_assets_detected() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "markdown").unwrap();
        let before = doc.doc_mut().get_heads();

        let mut assets = HashMap::new();
        assets.insert("image.png".to_string(), "sha256:abc123".to_string());
        let _ = doc.set_cell_resolved_assets("cell-1", &assets);
        let after = doc.doc_mut().get_heads();

        let changeset = diff_cells(doc.doc_mut(), &before, &after);
        assert_eq!(changeset.changed.len(), 1);
        assert!(changeset.changed[0].fields.resolved_assets);
    }

    #[test]
    fn test_extract_change_actors() {
        let mut daemon = NotebookDoc::new_with_actor("nb1", "runtimed");
        daemon.add_cell(0, "cell-1", "code").unwrap();

        let mut client = NotebookDoc::bootstrap(TextEncoding::UnicodeCodePoint, "human:test");
        sync_docs(&mut daemon, &mut client);

        let before = client.doc_mut().get_heads();

        daemon.update_source("cell-1", "x = 1").unwrap();
        sync_docs(&mut daemon, &mut client);

        let actors = extract_change_actors(client.doc_mut(), &before);
        assert!(actors.iter().any(|a| a.contains("runtimed")));
    }

    // ── diff_metadata_touched tests ─────────────────────────────────

    #[test]
    fn test_diff_metadata_touched_returns_false_for_cell_source_edit() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.update_source("cell-1", "print('hello')").unwrap();
        let after = doc.doc_mut().get_heads();

        assert!(!diff_metadata_touched(doc.doc_mut(), &before, &after));
    }

    #[test]
    fn test_diff_metadata_touched_returns_true_for_metadata_edit() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.set_metadata("runtime", "julia").unwrap();
        let after = doc.doc_mut().get_heads();

        assert!(diff_metadata_touched(doc.doc_mut(), &before, &after));
    }

    #[test]
    fn test_diff_metadata_touched_empty_before_returns_false() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let after = doc.doc_mut().get_heads();

        // Empty before = initial sync, should return false.
        assert!(!diff_metadata_touched(doc.doc_mut(), &[], &after));
    }

    #[test]
    fn test_diff_metadata_touched_returns_false_for_cell_metadata_edit() {
        let mut doc = NotebookDoc::new("nb1");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let before = doc.doc_mut().get_heads();

        doc.set_cell_source_hidden("cell-1", true).unwrap();
        let after = doc.doc_mut().get_heads();

        assert!(!diff_metadata_touched(doc.doc_mut(), &before, &after));
    }
}
