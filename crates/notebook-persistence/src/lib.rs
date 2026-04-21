#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! Shared `.ipynb` <-> `NotebookDoc` conversion.
//!
//! Previously lived in two copies:
//! * `runt/src/main.rs::doc_to_ipynb` (minimal version used by `runt recover`)
//! * `runtimed/src/notebook_sync_server.rs` (merge-aware save + parse helpers)
//!
//! Both paths now call into this crate so that any bit-level rules (source
//! line-splitting, nbformat_minor bumping, fallback cell IDs, etc.) live in
//! one place.
//!
//! The crate is WASM-friendly on default features. It depends only on
//! `automerge`, `notebook-doc`, `loro_fractional_index`, `serde`, and
//! `serde_json`.

pub mod error;
pub mod from_ipynb;
pub mod to_ipynb;

pub use error::{PersistenceError, Result};
pub use from_ipynb::{
    build_notebook_doc, parse_cells_from_ipynb, parse_metadata_from_ipynb,
    parse_nbformat_attachments_from_ipynb,
};
pub use to_ipynb::{build_ipynb, doc_to_ipynb, BuildError, BuildInputs, CellOutputData};
