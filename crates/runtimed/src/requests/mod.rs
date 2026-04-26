//! Per-request handler modules for the notebook sync server.
//!
//! `notebook_sync_server::handle_notebook_request` dispatches to these handlers
//! based on the `NotebookRequest` variant. Each module owns one variant's logic
//! so the dispatcher stays a thin match and each handler can be read in
//! isolation.
//!
//! Handlers accept references to the per-room state (`NotebookRoom`) and shared
//! daemon state (`Arc<Daemon>`) as parameters. They return `NotebookResponse`.
//! Shared helpers used by multiple handlers live in `helpers.rs`.
//!
//! This is a behavior-preserving split of the old 2k-line match statement —
//! lock scoping, log lines, error strings, and response variants are untouched.

pub(crate) mod check_tool_available;
pub(crate) mod clear_outputs;
pub(crate) mod clone_notebook;
pub(crate) mod complete;
pub(crate) mod execute_cell;
pub(crate) mod get_doc_bytes;
pub(crate) mod get_history;
pub(crate) mod get_kernel_info;
pub(crate) mod get_metadata_snapshot;
pub(crate) mod get_queue_state;
pub(crate) mod get_raw_metadata;
pub(crate) mod guarded;
pub(crate) mod interrupt_execution;
pub(crate) mod launch_kernel;
pub(crate) mod run_all_cells;
pub(crate) mod save_notebook;
pub(crate) mod send_comm;
pub(crate) mod set_metadata_snapshot;
pub(crate) mod set_raw_metadata;
pub(crate) mod shutdown_kernel;
pub(crate) mod sync_environment;
