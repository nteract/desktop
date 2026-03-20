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

    /// Fractional index hex string for ordering (e.g., "80", "7F80").
    /// Cells are sorted by this field.
    #[pyo3(get)]
    pub position: String,

    /// Cell source code/content
    #[pyo3(get)]
    pub source: String,

    /// Execution count (None if not executed)
    #[pyo3(get)]
    pub execution_count: Option<i64>,

    /// Cell outputs (resolved from automerge document)
    #[pyo3(get)]
    pub outputs: Vec<Output>,

    /// Cell metadata as JSON string (arbitrary JSON object)
    /// Access via metadata_json property, parse with json.loads() in Python
    #[pyo3(get)]
    pub metadata_json: String,
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

    /// Get metadata as a Python dict.
    ///
    /// Returns the parsed metadata object. Empty dict if no metadata.
    #[getter]
    fn metadata(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let json_module = py.import("json")?;
        let result = json_module.call_method1("loads", (&self.metadata_json,))?;
        Ok(result.unbind())
    }

    /// Check if source should be hidden (JupyterLab convention).
    #[getter]
    fn is_source_hidden(&self) -> bool {
        self.parsed_metadata()
            .and_then(|m| m.get("jupyter")?.get("source_hidden")?.as_bool())
            .unwrap_or(false)
    }

    /// Check if outputs should be hidden (JupyterLab convention).
    #[getter]
    fn is_outputs_hidden(&self) -> bool {
        self.parsed_metadata()
            .and_then(|m| m.get("jupyter")?.get("outputs_hidden")?.as_bool())
            .unwrap_or(false)
    }

    /// Check if cell is collapsed.
    #[getter]
    fn is_collapsed(&self) -> bool {
        self.parsed_metadata()
            .and_then(|m| m.get("collapsed")?.as_bool())
            .unwrap_or(false)
    }

    /// Get cell tags.
    #[getter]
    fn tags(&self) -> Vec<String> {
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

impl Cell {
    /// Parse metadata JSON string into a Value. Returns None if parsing fails.
    fn parsed_metadata(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.metadata_json).ok()
    }

    /// Create a Cell from a CellSnapshot without resolving outputs.
    /// Use `from_snapshot_with_outputs` to include resolved outputs.
    pub fn from_snapshot(snapshot: runtimed::notebook_doc::CellSnapshot) -> Self {
        // Parse execution_count from JSON string ("5" or "null")
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

    /// Create a Cell from a CellSnapshot with pre-resolved outputs.
    pub fn from_snapshot_with_outputs(
        snapshot: runtimed::notebook_doc::CellSnapshot,
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
}

/// An event from a streaming execution.
///
/// Events are yielded incrementally as a cell executes:
/// - "execution_started": execution began (has execution_count)
/// - "output": an output was produced (has output and optionally output_index)
/// - "done": execution finished
/// - "error": kernel error occurred (has error_message)
///
/// In signal-only mode, output events have output_index but no output data.
/// Use session.get_cell(cell_id).outputs[output_index] to fetch the data.
#[pyclass(skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct ExecutionEvent {
    /// Event type: "execution_started", "output", "done", "error"
    #[pyo3(get)]
    pub event_type: String,

    /// The cell ID this event is for
    #[pyo3(get)]
    pub cell_id: String,

    /// The output (only for "output" events, None in signal-only mode)
    #[pyo3(get)]
    pub output: Option<Output>,

    /// Index of the output in the cell's outputs list (for "output" events)
    #[pyo3(get)]
    pub output_index: Option<usize>,

    /// Execution count (only for "execution_started" events)
    #[pyo3(get)]
    pub execution_count: Option<i64>,

    /// Error message (only for "error" events)
    #[pyo3(get)]
    pub error_message: Option<String>,
}

#[pymethods]
impl ExecutionEvent {
    fn __repr__(&self) -> String {
        match self.event_type.as_str() {
            "output" => format!("ExecutionEvent(output, cell={})", self.cell_id),
            "execution_started" => format!(
                "ExecutionEvent(execution_started, cell={}, count={:?})",
                self.cell_id, self.execution_count
            ),
            "done" => format!("ExecutionEvent(done, cell={})", self.cell_id),
            "error" => format!(
                "ExecutionEvent(error, cell={}, msg={:?})",
                self.cell_id, self.error_message
            ),
            _ => format!("ExecutionEvent({}, cell={})", self.event_type, self.cell_id),
        }
    }
}

impl ExecutionEvent {
    pub fn execution_started(cell_id: &str, execution_count: i64) -> Self {
        Self {
            event_type: "execution_started".to_string(),
            cell_id: cell_id.to_string(),
            output: None,
            output_index: None,
            execution_count: Some(execution_count),
            error_message: None,
        }
    }

    pub fn output(cell_id: &str, output: Output) -> Self {
        Self {
            event_type: "output".to_string(),
            cell_id: cell_id.to_string(),
            output: Some(output),
            output_index: None,
            execution_count: None,
            error_message: None,
        }
    }

    /// Create an output event with the output index (for streaming).
    pub fn output_with_index(cell_id: &str, output: Output, output_index: Option<usize>) -> Self {
        Self {
            event_type: "output".to_string(),
            cell_id: cell_id.to_string(),
            output: Some(output),
            output_index,
            execution_count: None,
            error_message: None,
        }
    }

    /// Create a signal-only output event (output_index but no data).
    /// Used in signal_only mode where the consumer queries state for output data.
    pub fn output_signal(cell_id: &str, output_index: Option<usize>) -> Self {
        Self {
            event_type: "output".to_string(),
            cell_id: cell_id.to_string(),
            output: None,
            output_index,
            execution_count: None,
            error_message: None,
        }
    }

    pub fn done(cell_id: &str) -> Self {
        Self {
            event_type: "done".to_string(),
            cell_id: cell_id.to_string(),
            output: None,
            output_index: None,
            execution_count: None,
            error_message: None,
        }
    }

    pub fn error(cell_id: &str, message: &str) -> Self {
        Self {
            event_type: "error".to_string(),
            cell_id: cell_id.to_string(),
            output: None,
            output_index: None,
            execution_count: None,
            error_message: Some(message.to_string()),
        }
    }
}

/// A single completion item from the kernel.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct CompletionItem {
    /// The completion text
    pub label: String,
    /// Kind: "function", "variable", "class", "module", etc.
    pub kind: Option<String>,
    /// Short type annotation (e.g., "def read_csv(...)")
    pub detail: Option<String>,
    /// Source: "kernel"
    pub source: Option<String>,
}

#[pymethods]
impl CompletionItem {
    fn __repr__(&self) -> String {
        match &self.kind {
            Some(k) => format!("CompletionItem({}, kind={})", self.label, k),
            None => format!("CompletionItem({})", self.label),
        }
    }
}

impl CompletionItem {
    pub fn from_protocol(item: notebook_protocol::protocol::CompletionItem) -> Self {
        Self {
            label: item.label,
            kind: item.kind,
            detail: item.detail,
            source: item.source,
        }
    }
}

/// Result of a code completion request.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct CompletionResult {
    /// The completion items
    pub items: Vec<CompletionItem>,
    /// Cursor position where completions start
    pub cursor_start: usize,
    /// Cursor position where completions end
    pub cursor_end: usize,
}

#[pymethods]
impl CompletionResult {
    fn __repr__(&self) -> String {
        format!(
            "CompletionResult({} items, cursor={}..{})",
            self.items.len(),
            self.cursor_start,
            self.cursor_end
        )
    }
}

/// Current state of the execution queue.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct QueueState {
    /// Cell ID currently executing (None if idle)
    pub executing: Option<String>,
    /// Cell IDs waiting in queue
    pub queued: Vec<String>,
}

#[pymethods]
impl QueueState {
    fn __repr__(&self) -> String {
        match &self.executing {
            Some(cell_id) => format!(
                "QueueState(executing={}, queued={})",
                cell_id,
                self.queued.len()
            ),
            None => format!("QueueState(idle, queued={})", self.queued.len()),
        }
    }
}

/// A single entry from kernel input history.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Session number (0 for current session)
    pub session: i32,
    /// Line number within the session
    pub line: i32,
    /// The source code that was executed
    pub source: String,
}

#[pymethods]
impl HistoryEntry {
    fn __repr__(&self) -> String {
        let preview: String = self.source.chars().take(30).collect();
        let ellipsis = if self.source.len() > 30 { "..." } else { "" };
        format!(
            "HistoryEntry(session={}, line={}, source={:?}{})",
            self.session, self.line, preview, ellipsis
        )
    }
}

impl HistoryEntry {
    pub fn from_protocol(entry: notebook_protocol::protocol::HistoryEntry) -> Self {
        Self {
            session: entry.session,
            line: entry.line,
            source: entry.source,
        }
    }
}

/// Result of syncing environment with metadata.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct SyncEnvironmentResult {
    /// Whether the sync completed successfully
    pub success: bool,
    /// Packages that were installed (only if success=true)
    pub synced_packages: Vec<String>,
    /// Error message (only if success=false)
    pub error: Option<String>,
    /// Whether user should restart kernel instead (only if success=false)
    pub needs_restart: bool,
}

#[pymethods]
impl SyncEnvironmentResult {
    fn __repr__(&self) -> String {
        if self.success {
            format!(
                "SyncEnvironmentResult(success, packages={:?})",
                self.synced_packages
            )
        } else {
            format!(
                "SyncEnvironmentResult(failed, error={:?}, needs_restart={})",
                self.error, self.needs_restart
            )
        }
    }
}

/// Connection info returned when opening or creating a notebook via daemon.
///
/// This is returned by `Session.open_notebook()` and `Session.create_notebook()`
/// and provides information about the notebook that was opened or created.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct NotebookConnectionInfo {
    /// Protocol version (currently "v2").
    pub protocol: String,
    /// Numeric protocol version for explicit version checking.
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    pub daemon_version: Option<String>,
    /// Notebook identifier derived by the daemon.
    /// For existing files: canonical path.
    /// For new notebooks: generated UUID.
    pub notebook_id: String,
    /// Number of cells in the notebook.
    pub cell_count: usize,
    /// True if the notebook has untrusted dependencies requiring user approval.
    pub needs_trust_approval: bool,
}

#[pymethods]
impl NotebookConnectionInfo {
    fn __repr__(&self) -> String {
        let daemon_ver = self.daemon_version.as_deref().unwrap_or("unknown");
        format!(
            "NotebookConnectionInfo(notebook_id={}, cells={}, protocol_version={:?}, daemon_version={})",
            self.notebook_id, self.cell_count, self.protocol_version, daemon_ver
        )
    }
}

impl NotebookConnectionInfo {
    /// Create from the Rust NotebookConnectionInfo type.
    pub fn from_protocol(info: notebook_protocol::connection::NotebookConnectionInfo) -> Self {
        Self {
            protocol: info.protocol,
            protocol_version: info.protocol_version,
            daemon_version: info.daemon_version,
            notebook_id: info.notebook_id,
            cell_count: info.cell_count,
            needs_trust_approval: info.needs_trust_approval,
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

// ── Runtime state (from RuntimeStateDoc) ─────────────────────────────

/// Kernel state from the RuntimeStateDoc.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyKernelState {
    /// Kernel status: "not_started", "starting", "idle", "busy", "error", "shutdown"
    pub status: String,
    /// Kernel display name (e.g. "charming-toucan")
    pub name: String,
    /// Kernel language (e.g. "python", "typescript")
    pub language: String,
    /// Environment source label (e.g. "uv:prewarmed", "conda:pixi")
    pub env_source: String,
}

#[pymethods]
impl PyKernelState {
    fn __repr__(&self) -> String {
        format!(
            "KernelState(status={}, env_source={})",
            self.status, self.env_source
        )
    }
}

/// Environment sync state from the RuntimeStateDoc.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyEnvState {
    /// Whether notebook metadata matches the launched kernel config.
    pub in_sync: bool,
    /// Packages in metadata but not in the kernel environment.
    pub added: Vec<String>,
    /// Packages in the kernel environment but not in metadata.
    pub removed: Vec<String>,
    /// Whether conda channels differ.
    pub channels_changed: bool,
    /// Whether deno config differs.
    pub deno_changed: bool,
}

#[pymethods]
impl PyEnvState {
    fn __repr__(&self) -> String {
        if self.in_sync {
            "EnvState(in_sync)".to_string()
        } else {
            format!(
                "EnvState(drift: +{} -{} channels={} deno={})",
                self.added.len(),
                self.removed.len(),
                self.channels_changed,
                self.deno_changed,
            )
        }
    }
}

/// Full runtime state snapshot from the daemon's RuntimeStateDoc.
#[pyclass(get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyRuntimeState {
    /// Kernel state (status, name, language, env_source).
    pub kernel: PyKernelState,
    /// Execution queue state.
    pub queue: QueueState,
    /// Environment sync state.
    pub env: PyEnvState,
    /// ISO timestamp of last save, or None.
    pub last_saved: Option<String>,
}

#[pymethods]
impl PyRuntimeState {
    fn __repr__(&self) -> String {
        format!(
            "RuntimeState(kernel={}, queue={}, env={})",
            self.kernel.status,
            match &self.queue.executing {
                Some(id) => format!("executing={}", id),
                None => format!("idle, queued={}", self.queue.queued.len()),
            },
            if self.env.in_sync {
                "in_sync"
            } else {
                "drifted"
            },
        )
    }
}

impl From<notebook_doc::runtime_state::RuntimeState> for PyRuntimeState {
    fn from(rs: notebook_doc::runtime_state::RuntimeState) -> Self {
        Self {
            kernel: PyKernelState {
                status: rs.kernel.status,
                name: rs.kernel.name,
                language: rs.kernel.language,
                env_source: rs.kernel.env_source,
            },
            queue: QueueState {
                executing: rs.queue.executing,
                queued: rs.queue.queued,
            },
            env: PyEnvState {
                in_sync: rs.env.in_sync,
                added: rs.env.added,
                removed: rs.env.removed,
                channels_changed: rs.env.channels_changed,
                deno_changed: rs.env.deno_changed,
            },
            last_saved: rs.last_saved,
        }
    }
}
