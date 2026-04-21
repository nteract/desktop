//! Parse `.ipynb` JSON into the snapshots that the daemon uses to hydrate
//! a `NotebookDoc` and its side stores.
//!
//! These parsers intentionally do **not** create a `NotebookDoc` themselves.
//! The daemon's load path needs to interleave parsing with blob-store writes
//! and `RuntimeStateDoc` updates, so the pure "JSON -> snapshot" step is the
//! natural seam to share.
//!
//! There is no `build_notebook_doc`-style fully-assembled helper. Since
//! schema v3, cell outputs live in `RuntimeStateDoc` rather than the
//! notebook doc, and any "standalone" converter would silently drop
//! outputs. Callers that need a notebook doc with outputs must walk through
//! the daemon's load path (see `runtimed::notebook_sync_server`); callers
//! that only need snapshot data can combine these parsers themselves.

use std::collections::HashMap;

use loro_fractional_index::FractionalIndex;
use notebook_doc::metadata::NotebookMetadataSnapshot;
use notebook_doc::CellSnapshot;
use serde_json::Value;

/// Parse the `cells` array from a `.ipynb` JSON value into `CellSnapshot`s.
///
/// The source field can be either a string or an array of strings (lines);
/// both are normalized to a single `\n`-joined string.
///
/// For older notebooks (pre-nbformat 4.5) that don't carry cell IDs we
/// generate stable fallback IDs based on the cell index (`__external_cell_{i}`).
/// This prevents data loss when merging changes from externally-generated
/// notebooks.
///
/// Positions are generated incrementally using fractional indexing so bulk
/// loads are O(n) rather than O(n^2).
///
/// Returns `None` if the document is missing the `cells` key entirely.
pub fn parse_cells_from_ipynb(json: &Value) -> Option<Vec<CellSnapshot>> {
    let cells_json = json.get("cells").and_then(|c| c.as_array())?;

    // Generate positions incrementally
    let mut prev_position: Option<FractionalIndex> = None;

    let parsed_cells = cells_json
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            // Use existing ID or generate a stable fallback based on index.
            // This handles older notebooks (pre-nbformat 4.5) without cell IDs.
            let id = cell
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("__external_cell_{}", index));

            let cell_type = cell
                .get("cell_type")
                .and_then(|v| v.as_str())
                .unwrap_or("code")
                .to_string();

            // Generate position incrementally (O(1) per cell, not O(n^2))
            let position = match &prev_position {
                None => FractionalIndex::default(),
                Some(prev) => FractionalIndex::new_after(prev),
            };
            let position_str = position.to_string();
            prev_position = Some(position);

            // Source can be a string or array of strings
            let source = match cell.get("source") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };

            // Execution count: number or null (serialized as a JSON scalar string)
            let execution_count = match cell.get("execution_count") {
                Some(Value::Number(n)) => n.to_string(),
                _ => "null".to_string(),
            };

            // Outputs: keep as serde_json::Value for downstream manifest creation
            let outputs: Vec<Value> = cell
                .get("outputs")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Cell metadata (preserves all fields, normalized to object)
            let metadata = match cell.get("metadata") {
                Some(v) if v.is_object() => v.clone(),
                _ => serde_json::json!({}),
            };

            CellSnapshot {
                id,
                cell_type,
                position: position_str,
                source,
                execution_count,
                outputs,
                metadata,
                resolved_assets: std::collections::HashMap::new(),
            }
        })
        .collect();

    Some(parsed_cells)
}

/// Parse nbformat attachment payloads from a `.ipynb` JSON value.
///
/// Returns a map of `cell_id -> attachments JSON object` for any cell that
/// carries attachments. Cells without attachments are omitted. Fallback IDs
/// match [`parse_cells_from_ipynb`] so the two maps agree.
pub fn parse_nbformat_attachments_from_ipynb(json: &Value) -> HashMap<String, Value> {
    let Some(cells_json) = json.get("cells").and_then(|c| c.as_array()) else {
        return HashMap::new();
    };

    cells_json
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            let id = cell
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("__external_cell_{}", index));

            let attachments = cell.get("attachments")?;
            if attachments.is_object() {
                Some((id, attachments.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Parse notebook metadata from a `.ipynb` JSON value.
///
/// Extracts kernelspec, language_info, and the `runt` namespace from the
/// `metadata` object. Returns `None` if the input has no `metadata` key.
pub fn parse_metadata_from_ipynb(json: &Value) -> Option<NotebookMetadataSnapshot> {
    let metadata = json.get("metadata")?;
    Some(NotebookMetadataSnapshot::from_metadata_value(metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cells_from_ipynb_with_ids() {
        let json = serde_json::json!({
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "print('hello')",
                    "execution_count": 5,
                    "outputs": []
                },
                {
                    "id": "cell-2",
                    "cell_type": "markdown",
                    "source": ["# Title\n", "Body"],
                    "execution_count": null,
                    "outputs": []
                }
            ]
        });

        let cells = parse_cells_from_ipynb(&json).expect("should parse valid notebook");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[0].source, "print('hello')");
        assert_eq!(cells[0].execution_count, "5");
        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].cell_type, "markdown");
        assert_eq!(cells[1].source, "# Title\nBody");
        assert_eq!(cells[1].execution_count, "null");
    }

    #[test]
    fn parse_cells_from_ipynb_missing_ids() {
        // Older notebooks (pre-nbformat 4.5) don't have cell IDs
        let json = serde_json::json!({
            "cells": [
                { "cell_type": "code", "source": "x = 1", "execution_count": null, "outputs": [] },
                { "cell_type": "code", "source": "y = 2", "execution_count": null, "outputs": [] }
            ]
        });

        let cells = parse_cells_from_ipynb(&json).expect("should parse valid notebook");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "__external_cell_0");
        assert_eq!(cells[1].id, "__external_cell_1");
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[1].source, "y = 2");
    }

    #[test]
    fn parse_cells_from_ipynb_empty() {
        let json = serde_json::json!({ "cells": [] });
        let cells = parse_cells_from_ipynb(&json).expect("should parse valid empty notebook");
        assert!(cells.is_empty());
    }

    #[test]
    fn parse_cells_from_ipynb_no_cells_key() {
        let json = serde_json::json!({ "metadata": {} });
        assert!(
            parse_cells_from_ipynb(&json).is_none(),
            "should return None for invalid notebook"
        );
    }

    #[test]
    fn parse_attachments_from_ipynb_extracts_only_cells_with_attachments() {
        let json = serde_json::json!({
            "cells": [
                { "id": "c1", "cell_type": "markdown", "attachments": { "a.png": {"image/png": "d"} } },
                { "id": "c2", "cell_type": "markdown" },
                { "id": "c3", "cell_type": "markdown", "attachments": 5 }
            ]
        });
        let map = parse_nbformat_attachments_from_ipynb(&json);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("c1"));
    }
}
