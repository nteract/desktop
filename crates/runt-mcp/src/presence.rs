//! Presence helpers for the MCP server.
//!
//! Emits cursor positions and cell focus to the daemon so peers
//! (the notebook app) see where the agent is working. All presence
//! is best-effort — errors are silently ignored.

use notebook_doc::presence::{self, CursorPosition};
use notebook_sync::handle::DocHandle;

/// Peer label shown in the frontend presence UI.
const PEER_LABEL: &str = "Agent";

/// Emit a cursor position (line, column in a cell).
///
/// Shows a blinking cursor in the notebook app at the specified position.
pub async fn emit_cursor(handle: &DocHandle, cell_id: &str, line: u32, column: u32) {
    let data = presence::encode_cursor_update_labeled(
        "local",
        Some(PEER_LABEL),
        &CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    );
    let _ = handle.send_presence(data).await;
}

/// Emit a cell focus (agent is working on this cell, no specific cursor).
///
/// Shows a presence dot on the cell without a blinking cursor.
pub async fn emit_focus(handle: &DocHandle, cell_id: &str) {
    let data = presence::encode_focus_update_labeled("local", Some(PEER_LABEL), cell_id);
    let _ = handle.send_presence(data).await;
}

/// Convert a character offset in source to (line, column) — both 0-based.
/// Column is counted in Unicode code points (not bytes).
///
/// Matches the Python `offset_to_line_col()` implementation.
pub fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset.min(source.len())];
    let line = before.chars().filter(|&c| c == '\n').count() as u32;
    // Count characters (not bytes) after the last newline
    let after_last_newline = match before.rfind('\n') {
        Some(pos) => &before[pos + 1..],
        None => before,
    };
    let col = after_last_newline.chars().count() as u32;
    (line, col)
}
