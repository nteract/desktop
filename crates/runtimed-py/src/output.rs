//! Output types for execution results.

use pyo3::prelude::*;
use std::collections::HashMap;

/// A single output from cell execution.
#[pyclass(skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct Output {
    /// Output type: "stream", "display_data", "execute_result", "error"
    #[pyo3(get)]
    pub output_type: String,

    /// For stream outputs: "stdout" or "stderr"
    #[pyo3(get)]
    pub name: Option<String>,

    /// For stream outputs: the text content
    #[pyo3(get)]
    pub text: Option<String>,

    /// For display_data/execute_result: mime type -> content
    #[pyo3(get)]
    pub data: Option<HashMap<String, String>>,

    /// For errors: exception name
    #[pyo3(get)]
    pub ename: Option<String>,

    /// For errors: exception value
    #[pyo3(get)]
    pub evalue: Option<String>,

    /// For errors: traceback lines
    #[pyo3(get)]
    pub traceback: Option<Vec<String>>,

    /// For execute_result: execution count
    #[pyo3(get)]
    pub execution_count: Option<i64>,
}

#[pymethods]
impl Output {
    fn __repr__(&self) -> String {
        match self.output_type.as_str() {
            "stream" => format!(
                "Output(stream, {}: {:?})",
                self.name.as_deref().unwrap_or("?"),
                self.text.as_deref().unwrap_or("")
            ),
            "display_data" | "execute_result" => {
                let mime_types: Vec<&str> = self
                    .data
                    .as_ref()
                    .map(|d| d.keys().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                format!("Output({}, {:?})", self.output_type, mime_types)
            }
            "error" => format!(
                "Output(error, {}: {})",
                self.ename.as_deref().unwrap_or("?"),
                self.evalue.as_deref().unwrap_or("")
            ),
            _ => format!("Output({})", self.output_type),
        }
    }
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
        }
    }

    /// Create a display_data output.
    pub fn display_data(data: HashMap<String, String>) -> Self {
        Self {
            output_type: "display_data".to_string(),
            name: None,
            text: None,
            data: Some(data),
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: None,
        }
    }

    /// Create an execute_result output.
    pub fn execute_result(data: HashMap<String, String>, execution_count: i64) -> Self {
        Self {
            output_type: "execute_result".to_string(),
            name: None,
            text: None,
            data: Some(data),
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: Some(execution_count),
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
        }
    }
}

/// A cell from the automerge document.
#[pyclass(skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct Cell {
    /// Cell ID
    #[pyo3(get)]
    pub id: String,

    /// Cell type: "code", "markdown", or "raw"
    #[pyo3(get)]
    pub cell_type: String,

    /// Cell source code/content
    #[pyo3(get)]
    pub source: String,

    /// Execution count (None if not executed)
    #[pyo3(get)]
    pub execution_count: Option<i64>,

    /// Cell outputs (resolved from automerge document)
    #[pyo3(get)]
    pub outputs: Vec<Output>,
}

#[pymethods]
impl Cell {
    fn __repr__(&self) -> String {
        let preview: String = self.source.chars().take(30).collect();
        let ellipsis = if self.source.len() > 30 { "..." } else { "" };
        format!(
            "Cell(id={}, type={}, source={:?}{}, outputs={})",
            self.id,
            self.cell_type,
            preview,
            ellipsis,
            self.outputs.len()
        )
    }
}

impl Cell {
    /// Create a Cell from a CellSnapshot without resolving outputs.
    /// Use `from_snapshot_with_outputs` to include resolved outputs.
    pub fn from_snapshot(snapshot: runtimed::notebook_doc::CellSnapshot) -> Self {
        // Parse execution_count from JSON string ("5" or "null")
        let execution_count = snapshot.execution_count.parse::<i64>().ok();

        Self {
            id: snapshot.id,
            cell_type: snapshot.cell_type,
            source: snapshot.source,
            execution_count,
            outputs: Vec::new(),
        }
    }

    /// Create a Cell from a CellSnapshot with pre-resolved outputs.
    pub fn from_snapshot_with_outputs(
        snapshot: runtimed::notebook_doc::CellSnapshot,
        outputs: Vec<Output>,
    ) -> Self {
        let execution_count = snapshot.execution_count.parse::<i64>().ok();

        Self {
            id: snapshot.id,
            cell_type: snapshot.cell_type,
            source: snapshot.source,
            execution_count,
            outputs,
        }
    }
}

/// Result of executing code.
#[pyclass(skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct ExecutionResult {
    /// Cell ID that was executed
    #[pyo3(get)]
    pub cell_id: String,

    /// All outputs from execution
    #[pyo3(get)]
    pub outputs: Vec<Output>,

    /// Whether execution completed successfully (no error output)
    #[pyo3(get)]
    pub success: bool,

    /// Execution count (if available)
    #[pyo3(get)]
    pub execution_count: Option<i64>,
}

#[pymethods]
impl ExecutionResult {
    /// Get combined stdout text.
    #[getter]
    fn stdout(&self) -> String {
        self.outputs
            .iter()
            .filter(|o| o.output_type == "stream" && o.name.as_deref() == Some("stdout"))
            .filter_map(|o| o.text.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Get combined stderr text.
    #[getter]
    fn stderr(&self) -> String {
        self.outputs
            .iter()
            .filter(|o| o.output_type == "stream" && o.name.as_deref() == Some("stderr"))
            .filter_map(|o| o.text.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Get display data outputs (display_data and execute_result).
    #[getter]
    fn display_data(&self) -> Vec<Output> {
        self.outputs
            .iter()
            .filter(|o| o.output_type == "display_data" || o.output_type == "execute_result")
            .cloned()
            .collect()
    }

    /// Get error output if any.
    #[getter]
    fn error(&self) -> Option<Output> {
        self.outputs
            .iter()
            .find(|o| o.output_type == "error")
            .cloned()
    }

    fn __repr__(&self) -> String {
        let status = if self.success { "ok" } else { "error" };
        format!(
            "ExecutionResult(cell={}, status={}, outputs={})",
            self.cell_id,
            status,
            self.outputs.len()
        )
    }
}
