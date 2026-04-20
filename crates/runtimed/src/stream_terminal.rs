//! Terminal emulation for stream outputs (stdout/stderr).
//!
//! This module provides terminal emulation using `alacritty_terminal` to properly
//! handle escape sequences like carriage returns (for progress bars), backspaces,
//! and cursor movement. Each (execution_id, stream_name) pair gets its own terminal
//! emulator, and the rendered content is serialized back to ANSI text for the
//! frontend to display.

use std::collections::HashMap;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use alacritty_terminal::Term;

use crate::terminal_size::{TERMINAL_COLUMNS, TERMINAL_LINES};

/// Maximum scrollback history.
/// Keep minimal since notebook outputs don't need scrollback.
const SCROLLBACK_HISTORY: usize = 10000;

/// Key for terminal buffers: (execution_id, stream_name).
type StreamKey = (String, String);

// Re-export from the shared notebook-doc crate so existing callers
// (output_prep, notebook_sync_server) continue to compile.
pub use notebook_doc::StreamOutputState;

/// Simple dimensions struct for creating terminals.
struct TermDimensions {
    columns: usize,
    screen_lines: usize,
}

impl TermDimensions {
    fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns,
            screen_lines,
        }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Manages terminal emulators for stream outputs.
///
/// Each (execution_id, stream_name) pair gets its own terminal emulator to properly
/// handle escape sequences. When text is fed to a stream, it's processed through
/// the terminal and the rendered content is returned as ANSI text.
///
/// Also tracks the output state (index + manifest hash) for each stream to enable
/// efficient in-place updates with validation against external modifications.
pub struct StreamTerminals {
    terminals: HashMap<StreamKey, Term<VoidListener>>,
    processors: HashMap<StreamKey, Processor>,
    /// Output state for each (execution_id, stream_name) - tracks index and last hash for validation.
    output_states: HashMap<StreamKey, StreamOutputState>,
}

impl Default for StreamTerminals {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTerminals {
    /// Create a new StreamTerminals manager.
    pub fn new() -> Self {
        Self {
            terminals: HashMap::new(),
            processors: HashMap::new(),
            output_states: HashMap::new(),
        }
    }

    /// Feed text to the terminal for (execution_id, stream_name).
    ///
    /// Returns the rendered ANSI text representation of the terminal content.
    /// This handles escape sequences like `\r` (carriage return) and cursor
    /// movement, so progress bars will show only their final state.
    pub fn feed(&mut self, execution_id: &str, stream_name: &str, text: &str) -> String {
        let key = (execution_id.to_string(), stream_name.to_string());

        // Get or create terminal and processor for this stream
        let term = self.terminals.entry(key.clone()).or_insert_with(|| {
            let config = Config {
                scrolling_history: SCROLLBACK_HISTORY,
                ..Config::default()
            };
            let dimensions = TermDimensions::new(TERMINAL_COLUMNS, TERMINAL_LINES);
            Term::new(config, &dimensions, VoidListener)
        });

        let processor = self.processors.entry(key).or_default();

        // Feed input to terminal, converting \n to \r\n
        // A raw \n (line feed) only moves cursor down without returning to column 0.
        // We need \r\n to properly start at the beginning of the next line.
        for byte in text.as_bytes() {
            if *byte == b'\n' {
                processor.advance(term, b"\r\n");
            } else {
                processor.advance(term, std::slice::from_ref(byte));
            }
        }

        // Serialize terminal content back to ANSI text
        serialize_to_ansi(term)
    }

    /// Clear terminal(s) for an execution.
    ///
    /// Called when a non-stream output arrives to break the stream chain,
    /// or when clearing outputs for an execution.
    pub fn clear(&mut self, execution_id: &str) {
        // Remove all terminals for this execution (both stdout and stderr)
        self.terminals.retain(|(eid, _), _| eid != execution_id);
        self.processors.retain(|(eid, _), _| eid != execution_id);
        self.output_states.retain(|(eid, _), _| eid != execution_id);
    }

    /// Check if a stream exists for an execution.
    pub fn has_stream(&self, execution_id: &str, stream_name: &str) -> bool {
        let key = (execution_id.to_string(), stream_name.to_string());
        self.terminals.contains_key(&key)
    }

    /// Get the output state for a stream (if known).
    ///
    /// Returns the state (index + manifest hash) we last wrote for this stream.
    /// Used to validate before updating in place.
    pub fn get_output_state(
        &self,
        execution_id: &str,
        stream_name: &str,
    ) -> Option<&StreamOutputState> {
        let key = (execution_id.to_string(), stream_name.to_string());
        self.output_states.get(&key)
    }

    /// Set the output state for a stream.
    ///
    /// Called after upserting a stream output to track its position and hash
    /// for future validation.
    pub fn set_output_state(
        &mut self,
        execution_id: &str,
        stream_name: &str,
        state: StreamOutputState,
    ) {
        let key = (execution_id.to_string(), stream_name.to_string());
        self.output_states.insert(key, state);
    }
}

/// Serialize terminal content to ANSI-encoded string.
///
/// This iterates through the terminal's full grid (including scrollback history)
/// and converts it back to ANSI escape sequences that the frontend can render.
/// Only lines with actual content are included (trailing empty lines are trimmed).
fn serialize_to_ansi(term: &Term<VoidListener>) -> String {
    let grid = term.grid();
    let columns = grid.columns();
    let cursor_line = grid.cursor.point.line.0;

    // First pass: find the last line with actual content (across full history)
    let topmost = grid.topmost_line();
    let bottommost = grid.bottommost_line();
    let mut max_line_with_content: i32 = topmost.0 - 1; // Start below topmost

    for line_idx in topmost.0..=bottommost.0 {
        let row = &grid[Line(line_idx)];
        for col in 0..columns {
            let cell = &row[Column(col)];
            if cell.c != ' ' && cell.c != '\0' {
                max_line_with_content = max_line_with_content.max(line_idx);
                break; // Found content on this line, move to next
            }
        }
    }

    if max_line_with_content < topmost.0 {
        return String::new();
    }

    // Second pass: serialize with ANSI codes, only up to max_line_with_content
    let mut result = String::new();
    let mut current_fg: Option<Color> = None;
    let mut current_bg: Option<Color> = None;
    let mut current_flags = Flags::empty();
    let mut is_first_line = true;
    let mut has_emitted_styling = false; // Track if we've actually output any ANSI codes

    for line_idx in topmost.0..=max_line_with_content {
        let row = &grid[Line(line_idx)];

        // Find last column with meaningful content on this row
        // (non-space character, or space with styling that differs from default)
        let mut last_col_with_content: Option<usize> = None;
        for col in (0..columns).rev() {
            let cell = &row[Column(col)];
            // Skip spacer cells
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            // Check if this cell has content or styling
            let has_content = cell.c != ' ' && cell.c != '\0';
            let has_styling = !cell.flags.is_empty()
                || !matches!(cell.fg, Color::Named(NamedColor::Foreground))
                || !matches!(cell.bg, Color::Named(NamedColor::Background));
            if has_content || has_styling {
                last_col_with_content = Some(col);
                break;
            }
        }

        // Add newline between lines (not before first)
        if !is_first_line {
            result.push('\n');
        }
        is_first_line = false;

        // Only emit columns up to the last one with content
        let end_col = last_col_with_content.map(|c| c + 1).unwrap_or(0);
        for col in 0..end_col {
            let cell = &row[Column(col)];

            // Skip spacer cells for wide characters
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }

            // Emit attribute changes
            let mut attrs_changed = false;

            // Check if we need to reset — only if we've actually emitted styling.
            // Without this guard, wide-character spacer cells (emoji) can leave
            // stale attribute state that triggers a spurious \x1b[0m reset when
            // the next visible character has default attributes.
            let need_reset = has_emitted_styling
                && ((current_flags != cell.flags && !current_flags.is_empty())
                    || (current_fg.is_some() && current_fg != Some(cell.fg))
                    || (current_bg.is_some() && current_bg != Some(cell.bg)));

            if need_reset {
                result.push_str("\x1b[0m");
                current_fg = None;
                current_bg = None;
                current_flags = Flags::empty();
                has_emitted_styling = false;
                attrs_changed = true;
                // Note: reset doesn't count as "active styling" since it clears state
            }

            // Emit new flags
            if cell.flags != current_flags {
                if cell.flags.contains(Flags::BOLD) && !current_flags.contains(Flags::BOLD) {
                    result.push_str("\x1b[1m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                if cell.flags.contains(Flags::DIM) && !current_flags.contains(Flags::DIM) {
                    result.push_str("\x1b[2m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                if cell.flags.contains(Flags::ITALIC) && !current_flags.contains(Flags::ITALIC) {
                    result.push_str("\x1b[3m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                if cell.flags.contains(Flags::UNDERLINE)
                    && !current_flags.contains(Flags::UNDERLINE)
                {
                    result.push_str("\x1b[4m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                if cell.flags.contains(Flags::STRIKEOUT)
                    && !current_flags.contains(Flags::STRIKEOUT)
                {
                    result.push_str("\x1b[9m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                if cell.flags.contains(Flags::HIDDEN) && !current_flags.contains(Flags::HIDDEN) {
                    result.push_str("\x1b[8m");
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                current_flags = cell.flags;
            }

            // Emit foreground color if changed
            if current_fg != Some(cell.fg) {
                if let Some(ansi) = color_to_ansi(&cell.fg, true) {
                    result.push_str(&ansi);
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                current_fg = Some(cell.fg);
            }

            // Emit background color if changed
            if current_bg != Some(cell.bg) {
                if let Some(ansi) = color_to_ansi(&cell.bg, false) {
                    result.push_str(&ansi);
                    attrs_changed = true;
                    has_emitted_styling = true;
                }
                current_bg = Some(cell.bg);
            }

            // Emit the character
            if cell.c != ' ' || attrs_changed {
                result.push(cell.c);
            } else {
                result.push(' ');
            }

            // Emit any zero-width characters
            if let Some(zerowidth) = cell.zerowidth() {
                for c in zerowidth {
                    result.push(*c);
                }
            }
        }
    }

    // Reset at end only if we actually emitted styling codes
    if has_emitted_styling {
        result.push_str("\x1b[0m");
    }

    // Trim trailing whitespace from each line (safety net for edge cases)
    let lines: Vec<&str> = result.lines().collect();
    let trimmed_lines: Vec<String> = lines
        .iter()
        .map(|line| line.trim_end().to_string())
        .collect();

    // Remove trailing empty lines
    let mut final_lines = trimmed_lines;
    while final_lines.last().is_some_and(|l| l.is_empty()) {
        final_lines.pop();
    }

    let mut output = final_lines.join("\n");

    // Preserve trailing newline if cursor is on a line after the last content.
    // This happens when output ended with \n (e.g., print("hello") outputs "hello\n").
    if cursor_line > max_line_with_content && !output.is_empty() {
        output.push('\n');
    }

    output
}

/// Convert a Color to ANSI escape sequence.
fn color_to_ansi(color: &Color, is_foreground: bool) -> Option<String> {
    let base = if is_foreground { 30 } else { 40 };

    match color {
        Color::Named(named) => {
            let code = match named {
                NamedColor::Black => Some(base),
                NamedColor::Red => Some(base + 1),
                NamedColor::Green => Some(base + 2),
                NamedColor::Yellow => Some(base + 3),
                NamedColor::Blue => Some(base + 4),
                NamedColor::Magenta => Some(base + 5),
                NamedColor::Cyan => Some(base + 6),
                NamedColor::White => Some(base + 7),
                NamedColor::BrightBlack => Some(base + 60),
                NamedColor::BrightRed => Some(base + 61),
                NamedColor::BrightGreen => Some(base + 62),
                NamedColor::BrightYellow => Some(base + 63),
                NamedColor::BrightBlue => Some(base + 64),
                NamedColor::BrightMagenta => Some(base + 65),
                NamedColor::BrightCyan => Some(base + 66),
                NamedColor::BrightWhite => Some(base + 67),
                // Default foreground/background - don't emit
                NamedColor::Foreground | NamedColor::Background => None,
                // Other named colors (cursor, etc.) - skip
                _ => None,
            };
            code.map(|c| format!("\x1b[{}m", c))
        }
        Color::Spec(Rgb { r, g, b }) => {
            // True color (24-bit)
            let prefix = if is_foreground { 38 } else { 48 };
            Some(format!("\x1b[{};2;{};{};{}m", prefix, r, g, b))
        }
        Color::Indexed(idx) => {
            // 256-color palette
            let prefix = if is_foreground { 38 } else { 48 };
            Some(format!("\x1b[{};5;{}m", prefix, idx))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_text() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed("cell-1", "stdout", "hello world");
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_carriage_return() {
        let mut terminals = StreamTerminals::new();
        // Simulate progress bar: "Progress: 50%\rProgress: 100%"
        let result = terminals.feed("cell-1", "stdout", "Progress: 50%\rProgress: 100%");
        // Should only contain the final state
        assert!(result.contains("Progress: 100%"));
        assert!(!result.contains("Progress: 50%"));
    }

    #[test]
    fn test_newlines() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed("cell-1", "stdout", "line1\nline2\nline3");
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
    }

    #[test]
    fn test_colors() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed("cell-1", "stdout", "\x1b[31mred\x1b[0m normal");
        // Should preserve the ANSI codes
        assert!(result.contains("\x1b["));
        assert!(result.contains("red"));
        assert!(result.contains("normal"));
    }

    #[test]
    fn test_separate_streams() {
        let mut terminals = StreamTerminals::new();
        terminals.feed("cell-1", "stdout", "stdout content");
        terminals.feed("cell-1", "stderr", "stderr content");

        assert!(terminals.has_stream("cell-1", "stdout"));
        assert!(terminals.has_stream("cell-1", "stderr"));
    }

    #[test]
    fn test_clear() {
        let mut terminals = StreamTerminals::new();
        terminals.feed("cell-1", "stdout", "content");
        assert!(terminals.has_stream("cell-1", "stdout"));

        terminals.clear("cell-1");
        assert!(!terminals.has_stream("cell-1", "stdout"));
    }

    #[test]
    fn test_incremental_feed() {
        let mut terminals = StreamTerminals::new();

        // Feed in chunks like kernel would
        terminals.feed("cell-1", "stdout", "Hello ");
        let result = terminals.feed("cell-1", "stdout", "World!");

        assert!(result.contains("Hello World!"));
    }

    #[test]
    fn test_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<StreamTerminals>();
    }

    #[test]
    fn test_long_output_scrollback() {
        let mut terminals = StreamTerminals::new();

        // Create output longer than screen height (100 lines)
        // This tests that scrollback history is properly serialized
        let mut long_text = String::new();
        for i in 0..150 {
            long_text.push_str(&format!("Line {}\n", i));
        }

        let result = terminals.feed("cell-1", "stdout", &long_text);

        // Should contain all lines, not just the last 100
        assert!(
            result.contains("Line 0"),
            "Should contain Line 0 from scrollback"
        );
        assert!(result.contains("Line 50"), "Should contain Line 50");
        assert!(result.contains("Line 100"), "Should contain Line 100");
        assert!(result.contains("Line 149"), "Should contain Line 149");

        // Count the lines to verify none were truncated
        let line_count = result.lines().count();
        assert_eq!(line_count, 150, "Should have all 150 lines");
    }

    #[test]
    fn test_no_trailing_spaces() {
        let mut terminals = StreamTerminals::new();

        // Simple text should not have trailing spaces padding to terminal width
        let result = terminals.feed("cell-1", "stdout", "hello");

        // Plain text with no styling should be exactly "hello" - no reset code needed
        assert_eq!(result, "hello", "Plain text should have no ANSI codes");
    }

    #[test]
    fn test_styled_text_has_reset() {
        let mut terminals = StreamTerminals::new();

        // Text with ANSI color codes should have a reset at the end
        let result = terminals.feed("cell-1", "stdout", "\x1b[31mred\x1b[0m");

        // Should preserve the red color and have a reset at end
        assert!(result.contains("\x1b[31m"), "Should have red color code");
        assert!(result.ends_with("\x1b[0m"), "Should end with reset");
    }

    #[test]
    fn test_output_state_tracking() {
        let mut terminals = StreamTerminals::new();

        // Initially no state known
        assert!(terminals.get_output_state("cell-1", "stdout").is_none());

        // Set state after first upsert
        terminals.set_output_state(
            "cell-1",
            "stdout",
            StreamOutputState {
                index: 0,
                blob_hash: "hash1".to_string(),
            },
        );
        let state = terminals.get_output_state("cell-1", "stdout").unwrap();
        assert_eq!(state.index, 0);
        assert_eq!(state.blob_hash, "hash1");

        // Different stream gets different state
        terminals.set_output_state(
            "cell-1",
            "stderr",
            StreamOutputState {
                index: 1,
                blob_hash: "hash2".to_string(),
            },
        );
        assert_eq!(
            terminals
                .get_output_state("cell-1", "stderr")
                .unwrap()
                .index,
            1
        );
        assert_eq!(
            terminals
                .get_output_state("cell-1", "stdout")
                .unwrap()
                .index,
            0
        );

        // Update state (e.g., after new stream message)
        terminals.set_output_state(
            "cell-1",
            "stdout",
            StreamOutputState {
                index: 0,
                blob_hash: "hash3".to_string(),
            },
        );
        assert_eq!(
            terminals
                .get_output_state("cell-1", "stdout")
                .unwrap()
                .blob_hash,
            "hash3"
        );

        // Clear removes all state
        terminals.clear("cell-1");
        assert!(terminals.get_output_state("cell-1", "stdout").is_none());
        assert!(terminals.get_output_state("cell-1", "stderr").is_none());
    }

    #[test]
    fn test_emoji_no_spurious_ansi_reset() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed("cell-1", "stdout", "Ñoño → αβγδ → 🎵🎶 → ∑∏∫∂");
        // Plain unstyled text with emoji should have no ANSI escape codes at all
        assert!(
            !result.contains("\x1b["),
            "Unstyled emoji text should have no ANSI codes, got: {:?}",
            result
        );
        assert!(result.contains("🎵🎶 → ∑∏∫∂"));
    }

    #[test]
    fn test_emoji_with_ansi_styling_preserved() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed(
            "cell-1",
            "stdout",
            "\x1b[34m🔵 Blue emoji\x1b[0m → 🎶 no color → \x1b[31m🔴 Red emoji\x1b[0m",
        );
        // Blue styling should be present
        assert!(result.contains("\x1b[34m"), "Should have blue ANSI code");
        // Red styling should be present
        assert!(result.contains("\x1b[31m"), "Should have red ANSI code");
        // The unstyled region between resets should have no ANSI codes
        // Extract the middle section: after blue's reset, before red's start
        let after_blue_reset = result.split("→ 🎶").nth(1).unwrap_or("");
        let middle = after_blue_reset.split("→ \x1b[31m").next().unwrap_or("");
        assert!(
            !middle.contains("\x1b[0m"),
            "Unstyled region after emoji should not have spurious reset, got middle: {:?}",
            middle
        );
    }

    #[test]
    fn test_mixed_emoji_cjk_styled() {
        let mut terminals = StreamTerminals::new();
        let result = terminals.feed(
            "cell-1",
            "stdout",
            "\x1b[1;33m⚠️ Warning:\x1b[0m 日本語テスト 🦕🦖 → \x1b[4munderlined\x1b[0m → café ☕",
        );
        // Bold+yellow should be present
        assert!(result.contains("\x1b[1m"), "Should have bold code");
        assert!(result.contains("\x1b[33m"), "Should have yellow code");
        // Underline should be present
        assert!(result.contains("\x1b[4m"), "Should have underline code");
        // CJK and emoji should be preserved
        assert!(result.contains("日本語テスト"));
        assert!(result.contains("🦕🦖"));
        assert!(result.contains("café ☕"));
        // No spurious resets in unstyled regions
        let after_warning = result.split("日本語テスト").nth(1).unwrap_or("");
        let before_underline = after_warning.split("\x1b[4m").next().unwrap_or("");
        assert!(
            !before_underline.contains("\x1b[0m"),
            "Unstyled region between styled sections should not have spurious reset, got: {:?}",
            before_underline
        );
    }
}
