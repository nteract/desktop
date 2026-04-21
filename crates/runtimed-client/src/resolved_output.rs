//! Pure-Rust types for resolved notebook outputs and cells.
//!
//! These types are the canonical, framework-agnostic representations.
//! `runtimed-py` wraps them with PyO3 `#[pyclass]` for Python exposure;
//! `runt-mcp` uses them directly for MCP tool results.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A value in the output data dict, typed by MIME category.
///
/// | MIME type | Variant | Example |
/// |-----------|---------|---------|
/// | `text/*`, `image/svg+xml` | `Text` | `output.data["text/plain"]` |
/// | `image/png`, `audio/*`, ... | `Binary` | `output.data["image/png"]` |
/// | `application/json`, `*+json` | `Json` | `output.data["application/json"]` |
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum DataValue {
    /// UTF-8 text (text/*, image/svg+xml, etc.)
    Text(String),
    /// Raw binary bytes -- no base64 encoding (image/png, audio/*, etc.)
    Binary(Vec<u8>),
    /// Parsed JSON (application/json, application/*+json)
    Json(serde_json::Value),
}

/// A single output from cell execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Output {
    /// Output type: "stream", "display_data", "execute_result", "error"
    pub output_type: String,

    /// For stream outputs: "stdout" or "stderr"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// For stream outputs: the text content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// For display_data/execute_result: mime type -> content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<HashMap<String, DataValue>>,

    /// For errors: exception name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ename: Option<String>,

    /// For errors: exception value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evalue: Option<String>,

    /// For errors: traceback lines
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceback: Option<Vec<String>>,

    /// For execute_result: execution count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i64>,

    /// For display_data/execute_result: MIME type -> blob HTTP URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_urls: Option<HashMap<String, String>>,

    /// For display_data/execute_result: MIME type -> on-disk file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_paths: Option<HashMap<String, String>>,
}

impl Output {
    /// Create a stream output.
    pub fn stream(name: &str, text: &str) -> Self {
        Self {
            output_type: "stream".to_string(),
            name: Some(name.to_string()),
            text: Some(text.to_string()),
            data: None,
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: None,
            blob_urls: None,
            blob_paths: None,
        }
    }

    /// Create a display_data output.
    pub fn display_data(data: HashMap<String, DataValue>) -> Self {
        Self {
            output_type: "display_data".to_string(),
            name: None,
            text: None,
            data: Some(data),
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: None,
            blob_urls: None,
            blob_paths: None,
        }
    }

    /// Create an execute_result output.
    pub fn execute_result(data: HashMap<String, DataValue>, execution_count: i64) -> Self {
        Self {
            output_type: "execute_result".to_string(),
            name: None,
            text: None,
            data: Some(data),
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: Some(execution_count),
            blob_urls: None,
            blob_paths: None,
        }
    }

    /// Create an error output.
    pub fn error(ename: &str, evalue: &str, traceback: Vec<String>) -> Self {
        Self {
            output_type: "error".to_string(),
            name: None,
            text: None,
            data: None,
            ename: Some(ename.to_string()),
            evalue: Some(evalue.to_string()),
            traceback: Some(traceback),
            execution_count: None,
            blob_urls: None,
            blob_paths: None,
        }
    }
}

/// A cell with resolved outputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedCell {
    /// Cell ID
    pub id: String,
    /// Cell type: "code", "markdown", or "raw"
    pub cell_type: String,
    /// Fractional index hex string for ordering
    pub position: String,
    /// Cell source code/content
    pub source: String,
    /// Execution count (None if not executed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i64>,
    /// Resolved outputs
    pub outputs: Vec<Output>,
    /// Cell metadata as JSON string
    pub metadata_json: String,
}

impl ResolvedCell {
    /// Create from a CellSnapshot without outputs.
    pub fn from_snapshot(snapshot: notebook_doc::CellSnapshot) -> Self {
        let execution_count = snapshot.execution_count.parse::<i64>().ok();
        let metadata_json =
            serde_json::to_string(&snapshot.metadata).unwrap_or_else(|_| "{}".to_string());
        Self {
            id: snapshot.id,
            cell_type: snapshot.cell_type,
            position: snapshot.position,
            source: snapshot.source,
            execution_count,
            outputs: Vec::new(),
            metadata_json,
        }
    }

    /// Create from a CellSnapshot with pre-resolved outputs.
    pub fn from_snapshot_with_outputs(
        snapshot: notebook_doc::CellSnapshot,
        outputs: Vec<Output>,
    ) -> Self {
        let execution_count = snapshot.execution_count.parse::<i64>().ok();
        let metadata_json =
            serde_json::to_string(&snapshot.metadata).unwrap_or_else(|_| "{}".to_string());
        Self {
            id: snapshot.id,
            cell_type: snapshot.cell_type,
            position: snapshot.position,
            source: snapshot.source,
            execution_count,
            outputs,
            metadata_json,
        }
    }

    /// Parse metadata JSON string into a Value.
    pub fn parsed_metadata(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.metadata_json).ok()
    }

    /// Check if source should be hidden (JupyterLab convention).
    pub fn is_source_hidden(&self) -> bool {
        self.parsed_metadata()
            .and_then(|m| m.get("jupyter")?.get("source_hidden")?.as_bool())
            .unwrap_or(false)
    }

    /// Check if outputs should be hidden (JupyterLab convention).
    pub fn is_outputs_hidden(&self) -> bool {
        self.parsed_metadata()
            .and_then(|m| m.get("jupyter")?.get("outputs_hidden")?.as_bool())
            .unwrap_or(false)
    }

    /// Get cell tags.
    pub fn tags(&self) -> Vec<String> {
        self.parsed_metadata()
            .and_then(|m| {
                m.get("tags")?.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notebook_doc::CellSnapshot;
    use std::collections::HashMap;

    fn snapshot_with_metadata(metadata: serde_json::Value) -> CellSnapshot {
        CellSnapshot {
            id: "cell-1".into(),
            cell_type: "code".into(),
            position: "80".into(),
            source: "print('hi')".into(),
            execution_count: "null".into(),
            metadata,
            resolved_assets: HashMap::new(),
        }
    }

    #[test]
    fn from_snapshot_parses_execution_count() {
        // `execution_count` is stored as a JSON-ish string ("5", "null") for
        // CRDT-friendly scalar conflict resolution. ResolvedCell converts to
        // Option<i64> for the public API — "null" / non-numeric must become
        // None, not 0.
        let mut snap = snapshot_with_metadata(serde_json::json!({}));
        snap.execution_count = "5".into();
        let resolved = ResolvedCell::from_snapshot(snap);
        assert_eq!(resolved.execution_count, Some(5));

        let mut snap = snapshot_with_metadata(serde_json::json!({}));
        snap.execution_count = "null".into();
        let resolved = ResolvedCell::from_snapshot(snap);
        assert_eq!(resolved.execution_count, None);

        let mut snap = snapshot_with_metadata(serde_json::json!({}));
        snap.execution_count = "".into();
        let resolved = ResolvedCell::from_snapshot(snap);
        assert_eq!(resolved.execution_count, None);
    }

    #[test]
    fn from_snapshot_has_no_outputs() {
        // The base `from_snapshot` path is used when outputs aren't resolved
        // yet (e.g. during initial load). Must produce an empty vec, not a
        // default-placeholder with garbage.
        let resolved = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({})));
        assert!(resolved.outputs.is_empty());
    }

    #[test]
    fn from_snapshot_with_outputs_threads_them_through() {
        let outputs = vec![Output::stream("stdout", "hi")];
        let resolved = ResolvedCell::from_snapshot_with_outputs(
            snapshot_with_metadata(serde_json::json!({})),
            outputs.clone(),
        );
        assert_eq!(resolved.outputs.len(), 1);
        assert_eq!(resolved.outputs[0].output_type, "stream");
    }

    #[test]
    fn is_source_hidden_reads_jupyterlab_convention() {
        // JupyterLab stores this at `metadata.jupyter.source_hidden`. The
        // frontend depends on this to render collapsed code cells — regressing
        // would make every cell look expanded on load.
        let hidden = ResolvedCell::from_snapshot(snapshot_with_metadata(
            serde_json::json!({"jupyter": {"source_hidden": true}}),
        ));
        assert!(hidden.is_source_hidden());

        let shown = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({})));
        assert!(!shown.is_source_hidden());

        // Wrong type — must default to false, not panic.
        let bogus = ResolvedCell::from_snapshot(snapshot_with_metadata(
            serde_json::json!({"jupyter": {"source_hidden": "yes"}}),
        ));
        assert!(!bogus.is_source_hidden());
    }

    #[test]
    fn is_outputs_hidden_reads_jupyterlab_convention() {
        let hidden = ResolvedCell::from_snapshot(snapshot_with_metadata(
            serde_json::json!({"jupyter": {"outputs_hidden": true}}),
        ));
        assert!(hidden.is_outputs_hidden());

        let shown = ResolvedCell::from_snapshot(snapshot_with_metadata(
            serde_json::json!({"jupyter": {"outputs_hidden": false}}),
        ));
        assert!(!shown.is_outputs_hidden());

        let missing = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({})));
        assert!(!missing.is_outputs_hidden());
    }

    #[test]
    fn tags_reads_string_array_and_ignores_garbage() {
        // Tags is `metadata.tags`, an array of strings. A round-trip through a
        // .ipynb file may encounter non-string entries (malformed notebooks);
        // drop them rather than panicking.
        let cell = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({
            "tags": ["data", "slow", 42, null, "flaky"]
        })));
        assert_eq!(cell.tags(), vec!["data", "slow", "flaky"]);
    }

    #[test]
    fn tags_returns_empty_when_missing_or_wrong_type() {
        let missing = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({})));
        assert!(missing.tags().is_empty());

        let wrong =
            ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({"tags": "x"})));
        assert!(wrong.tags().is_empty());
    }

    #[test]
    fn parsed_metadata_returns_none_for_invalid_json() {
        let mut cell = ResolvedCell::from_snapshot(snapshot_with_metadata(serde_json::json!({})));
        // Defend the parsing path — if metadata_json is ever overwritten
        // with junk (from an older schema), downstream consumers should
        // see None, not panic.
        cell.metadata_json = "{not json".into();
        assert!(cell.parsed_metadata().is_none());
        assert!(!cell.is_source_hidden());
        assert!(cell.tags().is_empty());
    }
}
