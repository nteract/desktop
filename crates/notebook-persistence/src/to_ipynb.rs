//! Convert a `NotebookDoc` to `.ipynb` JSON.

use std::collections::HashMap;

use notebook_doc::metadata::NotebookMetadataSnapshot;
use notebook_doc::{CellSnapshot, NotebookDoc};
use serde_json::Value;

/// Split a cell source string into the multiline array format that
/// nbformat stores on disk. Each line keeps its trailing `\n`, except
/// the final line if the source does not end in a newline.
pub(crate) fn source_to_lines(source: &str) -> Vec<String> {
    if source.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut remaining = source;
    while let Some(pos) = remaining.find('\n') {
        lines.push(remaining[..=pos].to_string());
        remaining = &remaining[pos + 1..];
    }
    if !remaining.is_empty() {
        lines.push(remaining.to_string());
    }
    lines
}

/// Export a `NotebookDoc` directly to `.ipynb` JSON.
///
/// This is the minimal, synchronous conversion used by `runt recover`.
/// It reads cell source, per-cell metadata, and any outputs/execution counts
/// already embedded in the cell snapshots (pre-v3 docs) but does **not**
/// consult a `RuntimeStateDoc` or blob store. Callers that need live outputs
/// (e.g. the daemon's save path) should use [`build_ipynb`] with pre-resolved
/// outputs instead.
///
/// The output is always `nbformat: 4, nbformat_minor: 5`; no existing-file
/// metadata is merged in. For the daemon's merge-aware path, see
/// [`build_ipynb`].
pub fn doc_to_ipynb(doc: &NotebookDoc) -> Value {
    let cells = doc.get_cells();
    let metadata_snapshot = doc.get_metadata_snapshot();

    let nb_cells = cells
        .iter()
        .map(|cell| cell_to_ipynb_json(cell, None, None, None))
        .collect::<Vec<_>>();

    let mut metadata = serde_json::json!({});
    if let Some(snapshot) = metadata_snapshot.as_ref() {
        // merge_into_metadata_value only fails on serialize errors for the
        // runt namespace. Ignore here to preserve the CLI's best-effort
        // behavior; callers that care about diagnostics should use
        // build_ipynb which surfaces the error via the caller's logging.
        let _ = snapshot.merge_into_metadata_value(&mut metadata);
    }

    serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": metadata,
        "cells": nb_cells,
    })
}

/// Resolved outputs + execution count for a single cell, used by
/// [`build_ipynb`] when the caller has live data in a separate store
/// (e.g. the daemon's `RuntimeStateDoc`).
#[derive(Debug, Default)]
pub struct CellOutputData {
    /// Already-resolved output manifests (one JSON object per output).
    pub outputs: Vec<Value>,
    /// Execution count for the cell's most recent run, if known.
    pub execution_count: Option<i64>,
}

/// Inputs for the merge-aware build path used by the daemon.
///
/// Supplying `existing` preserves unknown top-level `metadata` keys and any
/// existing `nbformat_minor >= 5`. `outputs_by_cell_id` and
/// `attachments_by_cell_id` are looked up per cell ID to populate the
/// corresponding ipynb fields.
pub struct BuildInputs<'a> {
    /// Cell snapshots in document order (already sorted by position).
    pub cells: &'a [CellSnapshot],
    /// Notebook-level metadata snapshot (kernelspec, language_info, runt).
    pub metadata_snapshot: Option<&'a NotebookMetadataSnapshot>,
    /// Existing `.ipynb` JSON read from disk, used to preserve unknown
    /// metadata keys and the current `nbformat_minor`.
    pub existing: Option<&'a Value>,
    /// Per-cell output + execution_count data (keyed by cell ID). Cells that
    /// are not present default to empty outputs / null execution_count.
    pub outputs_by_cell_id: &'a HashMap<String, CellOutputData>,
    /// Per-cell `attachments` payloads for markdown/raw cells (keyed by cell ID).
    pub attachments_by_cell_id: &'a HashMap<String, Value>,
}

/// Errors from the merge-aware build path.
#[derive(Debug)]
pub enum BuildError {
    /// Failed to merge the notebook metadata snapshot into the metadata object.
    MetadataMerge(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::MetadataMerge(msg) => write!(f, "metadata merge failed: {msg}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Merge-aware construction of `.ipynb` JSON from a set of already-resolved
/// inputs.
///
/// Used by the daemon's save path so that outputs and execution counts from
/// `RuntimeStateDoc`, plus per-cell attachments from the room cache, end up
/// serialized together with cell source and metadata from the notebook doc.
///
/// Rules that callers typically rely on:
///
/// * `nbformat` is always `4`.
/// * `nbformat_minor` is `max(existing_minor, 5)` so cell IDs remain valid.
/// * Unknown top-level `metadata` keys from `existing` are preserved; the
///   notebook-level `metadata_snapshot` is merged on top.
/// * For code cells, outputs and execution_count come from `outputs_by_cell_id`.
/// * For markdown/raw cells, attachments come from `attachments_by_cell_id`.
pub fn build_ipynb(inputs: BuildInputs<'_>) -> std::result::Result<Value, BuildError> {
    let mut nb_cells = Vec::with_capacity(inputs.cells.len());
    for cell in inputs.cells {
        let output_data = inputs.outputs_by_cell_id.get(&cell.id);
        let attachments = inputs.attachments_by_cell_id.get(&cell.id);
        nb_cells.push(cell_to_ipynb_json(cell, output_data, attachments, None));
    }

    // Build metadata by merging the synced snapshot onto the existing file's
    // metadata (if any). Unknown keys on disk are preserved.
    let mut metadata = inputs
        .existing
        .and_then(|nb| nb.get("metadata"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if let Some(snapshot) = inputs.metadata_snapshot {
        snapshot
            .merge_into_metadata_value(&mut metadata)
            .map_err(|e| BuildError::MetadataMerge(e.to_string()))?;
    }

    // Cell IDs were introduced in nbformat 4.5, so ensure minor >= 5.
    let existing_minor = inputs
        .existing
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    Ok(serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": nbformat_minor,
        "metadata": metadata,
        "cells": nb_cells,
    }))
}

/// Build a single cell JSON, using output/execution data if the cell is a
/// code cell and a value is supplied. `attachments` is only honored for
/// markdown/raw cells.
fn cell_to_ipynb_json(
    cell: &CellSnapshot,
    output_data: Option<&CellOutputData>,
    attachments: Option<&Value>,
    _: Option<()>,
) -> Value {
    let source_lines = source_to_lines(&cell.source);

    let mut cell_json = serde_json::json!({
        "id": cell.id,
        "cell_type": cell.cell_type,
        "source": source_lines,
        "metadata": cell.metadata,
    });

    if cell.cell_type == "code" {
        match output_data {
            Some(data) => {
                cell_json["outputs"] = Value::Array(data.outputs.clone());
                cell_json["execution_count"] = data
                    .execution_count
                    .map(|n| Value::Number(serde_json::Number::from(n)))
                    .unwrap_or(Value::Null);
            }
            None => {
                // Fall back to whatever the cell snapshot carries. Pre-v3
                // docs stored outputs inline; post-v3 docs will produce an
                // empty vec and a null execution_count.
                cell_json["outputs"] = Value::Array(cell.outputs.clone());
                let exec_count: Value =
                    serde_json::from_str(&cell.execution_count).unwrap_or(Value::Null);
                cell_json["execution_count"] = exec_count;
            }
        }
    } else if matches!(cell.cell_type.as_str(), "markdown" | "raw") {
        if let Some(att) = attachments {
            cell_json["attachments"] = att.clone();
        }
    }

    cell_json
}

#[cfg(test)]
mod tests {
    use super::*;
    use notebook_doc::NotebookDoc;

    #[test]
    fn doc_to_ipynb_basic() {
        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "print('hello')").unwrap();
        doc.add_cell(1, "cell-2", "markdown").unwrap();
        doc.update_source("cell-2", "# Title").unwrap();

        let result = doc_to_ipynb(&doc);

        assert_eq!(result["nbformat"], 4);
        assert_eq!(result["nbformat_minor"], 5);
        assert!(result["metadata"].is_object());

        let cells = result["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);

        // Code cell
        assert_eq!(cells[0]["cell_type"], "code");
        assert_eq!(cells[0]["id"], "cell-1");
        let source: Vec<&str> = cells[0]["source"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(source.join(""), "print('hello')");
        assert!(cells[0]["outputs"].as_array().unwrap().is_empty());
        assert!(cells[0]["execution_count"].is_null());

        // Markdown cell — no outputs or execution_count keys
        assert_eq!(cells[1]["cell_type"], "markdown");
        assert_eq!(cells[1]["id"], "cell-2");
        assert!(cells[1].get("outputs").is_none());
        assert!(cells[1].get("execution_count").is_none());
    }

    #[test]
    fn doc_to_ipynb_multiline_source() {
        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "line1\nline2\nline3").unwrap();

        let result = doc_to_ipynb(&doc);
        let source: Vec<&str> = result["cells"][0]["source"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(source, vec!["line1\n", "line2\n", "line3"]);
    }

    #[test]
    fn doc_to_ipynb_empty_source() {
        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();

        let result = doc_to_ipynb(&doc);
        let source = result["cells"][0]["source"].as_array().unwrap();
        assert!(source.is_empty());
    }

    #[test]
    fn source_to_lines_matches_nbformat_convention() {
        assert!(source_to_lines("").is_empty());
        assert_eq!(source_to_lines("hello"), vec!["hello"]);
        assert_eq!(source_to_lines("a\n"), vec!["a\n"]);
        assert_eq!(source_to_lines("a\nb\nc"), vec!["a\n", "b\n", "c"]);
        assert_eq!(source_to_lines("a\nb\n"), vec!["a\n", "b\n"]);
    }

    #[test]
    fn build_ipynb_promotes_nbformat_minor_to_5() {
        let existing = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 2,
            "metadata": {},
            "cells": []
        });
        let outputs = HashMap::new();
        let attachments = HashMap::new();
        let result = build_ipynb(BuildInputs {
            cells: &[],
            metadata_snapshot: None,
            existing: Some(&existing),
            outputs_by_cell_id: &outputs,
            attachments_by_cell_id: &attachments,
        })
        .unwrap();
        assert_eq!(result["nbformat_minor"], 5);
    }

    #[test]
    fn build_ipynb_preserves_higher_nbformat_minor() {
        let existing = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 7,
            "metadata": {},
            "cells": []
        });
        let outputs = HashMap::new();
        let attachments = HashMap::new();
        let result = build_ipynb(BuildInputs {
            cells: &[],
            metadata_snapshot: None,
            existing: Some(&existing),
            outputs_by_cell_id: &outputs,
            attachments_by_cell_id: &attachments,
        })
        .unwrap();
        assert_eq!(result["nbformat_minor"], 7);
    }

    #[test]
    fn build_ipynb_preserves_unknown_existing_metadata_keys() {
        let existing = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {
                "custom_tool": { "version": "1.2.3" },
                "authors": [{ "name": "Kyle" }]
            },
            "cells": []
        });
        let outputs = HashMap::new();
        let attachments = HashMap::new();
        let result = build_ipynb(BuildInputs {
            cells: &[],
            metadata_snapshot: None,
            existing: Some(&existing),
            outputs_by_cell_id: &outputs,
            attachments_by_cell_id: &attachments,
        })
        .unwrap();
        assert_eq!(result["metadata"]["custom_tool"]["version"], "1.2.3");
        assert_eq!(result["metadata"]["authors"][0]["name"], "Kyle");
    }

    #[test]
    fn build_ipynb_uses_runtime_outputs_for_code_cells() {
        let cell = CellSnapshot {
            id: "c1".into(),
            cell_type: "code".into(),
            position: "80".into(),
            source: "x".into(),
            execution_count: "null".into(),
            outputs: vec![],
            metadata: serde_json::json!({}),
            resolved_assets: Default::default(),
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "c1".into(),
            CellOutputData {
                outputs: vec![serde_json::json!({ "output_type": "stream", "text": "hi" })],
                execution_count: Some(3),
            },
        );
        let attachments = HashMap::new();
        let result = build_ipynb(BuildInputs {
            cells: std::slice::from_ref(&cell),
            metadata_snapshot: None,
            existing: None,
            outputs_by_cell_id: &outputs,
            attachments_by_cell_id: &attachments,
        })
        .unwrap();
        assert_eq!(result["cells"][0]["execution_count"], 3);
        assert_eq!(result["cells"][0]["outputs"][0]["output_type"], "stream");
    }

    #[test]
    fn build_ipynb_attaches_markdown_attachments() {
        let cell = CellSnapshot {
            id: "c1".into(),
            cell_type: "markdown".into(),
            position: "80".into(),
            source: "![x](attachment:image.png)".into(),
            execution_count: "null".into(),
            outputs: vec![],
            metadata: serde_json::json!({}),
            resolved_assets: Default::default(),
        };
        let outputs = HashMap::new();
        let mut attachments = HashMap::new();
        attachments.insert(
            "c1".into(),
            serde_json::json!({ "image.png": { "image/png": "base64data" } }),
        );
        let result = build_ipynb(BuildInputs {
            cells: std::slice::from_ref(&cell),
            metadata_snapshot: None,
            existing: None,
            outputs_by_cell_id: &outputs,
            attachments_by_cell_id: &attachments,
        })
        .unwrap();
        assert_eq!(
            result["cells"][0]["attachments"]["image.png"]["image/png"],
            "base64data"
        );
    }
}
