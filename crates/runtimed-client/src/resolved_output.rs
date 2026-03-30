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
