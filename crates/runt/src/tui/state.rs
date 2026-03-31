use std::path::PathBuf;

use notebook_doc::CellSnapshot;
use notebook_protocol::protocol::NotebookBroadcast;
use runtimed::output_resolver::{output_from_json, resolve_cell_outputs};
use runtimed::resolved_output::Output;

/// Current interaction mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Edit,
}

/// Cursor position in the editor.
#[derive(Clone, Debug, Default)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
}

/// Application state for the TUI.
pub struct App {
    pub cells: Vec<CellView>,
    pub selected: usize,
    pub scroll_offset: u16,
    pub kernel_status: String,
    pub kernel_language: String,
    pub notebook_path: String,
    pub should_quit: bool,
    pub mode: Mode,
    pub cursor: Cursor,
    /// Working copy of the source being edited (separate from the cell source
    /// so we can discard on Esc without writing back).
    pub edit_buffer: Vec<String>,
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
            mode: Mode::Normal,
            cursor: Cursor::default(),
            edit_buffer: Vec::new(),
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

    /// Rebuild cell views from a fresh document snapshot (e.g. when a remote peer adds/removes cells).
    pub async fn apply_snapshot(
        &mut self,
        snapshot: notebook_sync::NotebookSnapshot,
        blob_base_url: &Option<String>,
        blob_store_path: &Option<PathBuf>,
    ) {
        let mut cell_views = Vec::with_capacity(snapshot.cells.len());
        for cell_snapshot in snapshot.cells.iter().cloned() {
            let outputs =
                resolve_cell_outputs(&cell_snapshot.outputs, blob_base_url, blob_store_path).await;
            cell_views.push(CellView::from_snapshot(cell_snapshot, outputs));
        }
        self.cells = cell_views;
        // Clamp selection
        if self.selected >= self.cells.len() && !self.cells.is_empty() {
            self.selected = self.cells.len() - 1;
        }
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

    // ── Edit mode ──────────────────────────────────────────────

    /// Enter edit mode for the selected cell.
    pub fn enter_edit_mode(&mut self) {
        if let Some(cell) = self.cells.get(self.selected) {
            self.edit_buffer = cell.source.lines().map(String::from).collect();
            if self.edit_buffer.is_empty() {
                self.edit_buffer.push(String::new());
            }
            self.cursor = Cursor { line: 0, col: 0 };
            self.mode = Mode::Edit;
        }
    }

    /// Exit edit mode, syncing the local cell source from the edit buffer.
    pub fn exit_edit_mode(&mut self) -> Option<String> {
        if self.mode != Mode::Edit {
            return None;
        }
        let new_source = self.edit_buffer.join("\n");
        if let Some(cell) = self.cells.get_mut(self.selected) {
            cell.source = new_source;
        }
        self.mode = Mode::Normal;
        self.selected_cell_id().map(|s| s.to_string())
    }

    /// Get the flat char offset of the cursor in the edit buffer.
    /// Lines are joined by '\n', so line N starts at sum of (len of lines 0..N) + N newlines.
    fn cursor_char_offset(&self) -> usize {
        let mut offset = 0;
        for (i, line) in self.edit_buffer.iter().enumerate() {
            if i == self.cursor.line {
                offset += self.cursor.col;
                break;
            }
            offset += line.chars().count() + 1; // +1 for '\n'
        }
        offset
    }

    /// Insert a character at the cursor. Returns a splice op.
    pub fn edit_insert_char(&mut self, c: char) -> Option<Splice> {
        let offset = self.cursor_char_offset();
        if let Some(line) = self.edit_buffer.get_mut(self.cursor.line) {
            let byte_idx = char_to_byte_index(line, self.cursor.col);
            line.insert(byte_idx, c);
            self.cursor.col += 1;
            return Some(Splice {
                index: offset,
                delete_count: 0,
                text: c.to_string(),
            });
        }
        None
    }

    /// Insert a newline at the cursor. Returns a splice op.
    pub fn edit_insert_newline(&mut self) -> Option<Splice> {
        let offset = self.cursor_char_offset();
        if let Some(line) = self.edit_buffer.get(self.cursor.line) {
            let byte_idx = char_to_byte_index(line, self.cursor.col);
            let rest = line[byte_idx..].to_string();
            self.edit_buffer[self.cursor.line] = line[..byte_idx].to_string();
            self.edit_buffer.insert(self.cursor.line + 1, rest);
            self.cursor.line += 1;
            self.cursor.col = 0;
            return Some(Splice {
                index: offset,
                delete_count: 0,
                text: "\n".to_string(),
            });
        }
        None
    }

    /// Delete the character before the cursor (backspace). Returns a splice op.
    pub fn edit_backspace(&mut self) -> Option<Splice> {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
            let offset = self.cursor_char_offset();
            if let Some(line) = self.edit_buffer.get_mut(self.cursor.line) {
                let byte_idx = char_to_byte_index(line, self.cursor.col);
                let next_byte_idx = char_to_byte_index(line, self.cursor.col + 1);
                line.replace_range(byte_idx..next_byte_idx, "");
                return Some(Splice {
                    index: offset,
                    delete_count: 1,
                    text: String::new(),
                });
            }
        } else if self.cursor.line > 0 {
            // Join with previous line — delete the '\n' before this line
            self.cursor.line -= 1;
            self.cursor.col = self.edit_buffer[self.cursor.line].chars().count();
            let offset = self.cursor_char_offset();
            let current = self.edit_buffer.remove(self.cursor.line + 1);
            self.edit_buffer[self.cursor.line].push_str(&current);
            return Some(Splice {
                index: offset,
                delete_count: 1,
                text: String::new(),
            });
        }
        None
    }

    /// Delete the character at the cursor (delete key). Returns a splice op.
    pub fn edit_delete(&mut self) -> Option<Splice> {
        let offset = self.cursor_char_offset();
        if let Some(line) = self.edit_buffer.get_mut(self.cursor.line) {
            let char_count = line.chars().count();
            if self.cursor.col < char_count {
                let byte_idx = char_to_byte_index(line, self.cursor.col);
                let next_byte_idx = char_to_byte_index(line, self.cursor.col + 1);
                line.replace_range(byte_idx..next_byte_idx, "");
                return Some(Splice {
                    index: offset,
                    delete_count: 1,
                    text: String::new(),
                });
            } else if self.cursor.line + 1 < self.edit_buffer.len() {
                // Join with next line — delete the '\n'
                let next = self.edit_buffer.remove(self.cursor.line + 1);
                self.edit_buffer[self.cursor.line].push_str(&next);
                return Some(Splice {
                    index: offset,
                    delete_count: 1,
                    text: String::new(),
                });
            }
        }
        None
    }

    /// Insert multiple characters (e.g. tab = 4 spaces). Returns a splice op.
    pub fn edit_insert_str(&mut self, s: &str) -> Option<Splice> {
        let offset = self.cursor_char_offset();
        if let Some(line) = self.edit_buffer.get_mut(self.cursor.line) {
            let byte_idx = char_to_byte_index(line, self.cursor.col);
            line.insert_str(byte_idx, s);
            self.cursor.col += s.chars().count();
            return Some(Splice {
                index: offset,
                delete_count: 0,
                text: s.to_string(),
            });
        }
        None
    }

    /// Move cursor in edit mode.
    pub fn edit_move_cursor(&mut self, dir: CursorDir) {
        match dir {
            CursorDir::Left => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            CursorDir::Right => {
                if let Some(line) = self.edit_buffer.get(self.cursor.line) {
                    if self.cursor.col < line.chars().count() {
                        self.cursor.col += 1;
                    }
                }
            }
            CursorDir::Up => {
                if self.cursor.line > 0 {
                    self.cursor.line -= 1;
                    self.clamp_cursor_col();
                }
            }
            CursorDir::Down => {
                if self.cursor.line + 1 < self.edit_buffer.len() {
                    self.cursor.line += 1;
                    self.clamp_cursor_col();
                }
            }
            CursorDir::Home => {
                self.cursor.col = 0;
            }
            CursorDir::End => {
                if let Some(line) = self.edit_buffer.get(self.cursor.line) {
                    self.cursor.col = line.chars().count();
                }
            }
        }
    }

    fn clamp_cursor_col(&mut self) {
        if let Some(line) = self.edit_buffer.get(self.cursor.line) {
            let max_col = line.chars().count();
            if self.cursor.col > max_col {
                self.cursor.col = max_col;
            }
        }
    }
}

/// A splice operation to send to the CRDT.
pub struct Splice {
    pub index: usize,
    pub delete_count: usize,
    pub text: String,
}

pub enum CursorDir {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Convert a char-based column to a byte index in a string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}
