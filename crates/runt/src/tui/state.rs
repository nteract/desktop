use std::path::PathBuf;

use notebook_doc::CellSnapshot;
use notebook_protocol::protocol::NotebookBroadcast;
use runtimed::output_resolver::{output_from_json, resolve_cell_outputs};
use runtimed::resolved_output::Output;

/// Application state for the TUI.
pub struct App {
    pub cells: Vec<CellView>,
    pub selected: usize,
    pub scroll_offset: u16,
    pub kernel_status: String,
    pub kernel_language: String,
    pub notebook_path: String,
    pub should_quit: bool,
    _blob_base_url: Option<String>,
    _blob_store_path: Option<PathBuf>,
}

/// A cell ready for display.
pub struct CellView {
    pub id: String,
    pub cell_type: String,
    pub source: String,
    pub execution_count: Option<i64>,
    pub outputs: Vec<Output>,
}

impl CellView {
    fn from_snapshot(snapshot: CellSnapshot, outputs: Vec<Output>) -> Self {
        let execution_count = snapshot.execution_count.parse::<i64>().ok();
        Self {
            id: snapshot.id,
            cell_type: snapshot.cell_type,
            source: snapshot.source,
            execution_count,
            outputs,
        }
    }

    /// Extract displayable text from an output.
    pub fn output_text(output: &Output) -> String {
        match output.output_type.as_str() {
            "stream" => output.text.clone().unwrap_or_default(),
            "execute_result" | "display_data" => {
                if let Some(data) = &output.data {
                    if let Some(runtimed::resolved_output::DataValue::Text(s)) =
                        data.get("text/plain")
                    {
                        return s.clone();
                    }
                }
                String::new()
            }
            "error" => {
                let mut lines = Vec::new();
                if let (Some(ename), Some(evalue)) = (&output.ename, &output.evalue) {
                    lines.push(format!("{}: {}", ename, evalue));
                }
                if let Some(tb) = &output.traceback {
                    lines.extend(tb.iter().cloned());
                }
                lines.join("\n")
            }
            _ => String::new(),
        }
    }
}

impl App {
    /// Build initial state from cell snapshots, resolving outputs.
    pub async fn from_cells(
        cells: Vec<CellSnapshot>,
        path: &str,
        blob_base_url: &Option<String>,
        blob_store_path: &Option<PathBuf>,
    ) -> Self {
        let mut cell_views = Vec::with_capacity(cells.len());
        for snapshot in cells {
            let outputs =
                resolve_cell_outputs(&snapshot.outputs, blob_base_url, blob_store_path).await;
            cell_views.push(CellView::from_snapshot(snapshot, outputs));
        }
        Self {
            cells: cell_views,
            selected: 0,
            scroll_offset: 0,
            kernel_status: "not_started".to_string(),
            kernel_language: String::new(),
            notebook_path: path.to_string(),
            should_quit: false,
            _blob_base_url: blob_base_url.clone(),
            _blob_store_path: blob_store_path.clone(),
        }
    }

    /// Apply a broadcast event from the daemon.
    pub fn apply_broadcast(&mut self, broadcast: NotebookBroadcast) {
        match broadcast {
            NotebookBroadcast::KernelStatus { status, .. } => {
                self.kernel_status = status;
            }
            NotebookBroadcast::ExecutionStarted {
                cell_id,
                execution_count,
                ..
            } => {
                if let Some(cell) = self.cells.iter_mut().find(|c| c.id == cell_id) {
                    cell.execution_count = Some(execution_count);
                    cell.outputs.clear();
                }
            }
            NotebookBroadcast::Output {
                cell_id,
                output_json,
                output_index,
                ..
            } => {
                if let Some(cell) = self.cells.iter_mut().find(|c| c.id == cell_id) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output_json) {
                        if let Some(output_type) =
                            parsed.get("output_type").and_then(|v| v.as_str())
                        {
                            if let Some(output) = output_from_json(output_type, &parsed) {
                                if let Some(idx) = output_index {
                                    if idx < cell.outputs.len() {
                                        cell.outputs[idx] = output;
                                    } else {
                                        cell.outputs.push(output);
                                    }
                                } else {
                                    cell.outputs.push(output);
                                }
                            }
                        }
                    }
                }
            }
            NotebookBroadcast::OutputsCleared { cell_id } => {
                if let Some(cell) = self.cells.iter_mut().find(|c| c.id == cell_id) {
                    cell.outputs.clear();
                }
            }
            NotebookBroadcast::KernelError { error } => {
                self.kernel_status = format!("error: {}", error);
            }
            _ => {}
        }
    }

    /// Update kernel status from RuntimeState.
    pub fn update_runtime_state(&mut self, state: notebook_doc::runtime_state::RuntimeState) {
        self.kernel_status = state.kernel.status;
        self.kernel_language = state.kernel.language;
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.cells.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        if !self.cells.is_empty() {
            self.selected = self.cells.len() - 1;
        }
    }

    pub fn selected_cell_id(&self) -> Option<&str> {
        self.cells.get(self.selected).map(|c| c.id.as_str())
    }
}
