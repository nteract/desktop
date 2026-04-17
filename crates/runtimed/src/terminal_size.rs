//! Terminal size constants for kernel output.
//!
//! These constants define the terminal dimensions used for:
//! 1. Kernel environment variables (COLUMNS, LINES) - what subprocesses see
//! 2. Stream terminal emulation - how we process escape sequences
//!
//! Both values should match to ensure consistent output formatting between
//! what the kernel produces and how we render it.

pub const TERMINAL_COLUMNS: usize = 128;
pub const TERMINAL_LINES: usize = 100;
pub const TERMINAL_COLUMNS_STR: &str = "128";
pub const TERMINAL_LINES_STR: &str = "100";
