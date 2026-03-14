//! Automerge-based notebook sync client with direct document access.
//!
//! Inspired by [samod](https://github.com/alexjg/samod) (automerge-repo in Rust),
//! this crate provides a `DocHandle` that gives callers direct, synchronous access
//! to the Automerge document via `with_doc`. No command channels, no serialization,
//! no async overhead for document mutations.
//!
//! ## Architecture
//!
//! ```text
//! DocHandle (callers)                    SyncTask (network I/O)
//!   │                                      │
//!   ├─ handle.add_cell_after(...)           │
//!   │   (convenience method)               │
//!   │                                      │
//!   ├─ handle.with_doc(|doc| { ... })      │
//!   │   → lock mutex                       │
//!   │   → mutate &mut AutoCommit           │
//!   │   → publish snapshot                 │
//!   │   → notify sync task ──────────────► │ generate_sync_message()
//!   │                                      │ → send to daemon
//!   │                                      │
//!   ├─ snapshot()                           │
//!   │   → read watch channel (no lock)     │
//!   │                                      │
//!   ├─ send_request(req).await ───────────►│ socket write/read
//!   │   (async — needs socket I/O)         │
//! ```
//!
//! For single operations, use convenience methods like `add_cell_after`,
//! `update_source`, `set_metadata_string`, etc. For compound operations
//! that should be atomic, use `with_doc` directly with `NotebookDoc::wrap`:
//!
//! ```ignore
//! // Single operation — convenience method
//! handle.add_cell_after("cell-1", "code", None)?;
//!
//! // Compound operation — with_doc for atomicity
//! handle.with_doc(|doc| {
//!     let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
//!     nd.add_cell_after("cell-1", "code", None)?;
//!     nd.update_source("cell-1", "print('hello')")?;
//!     *doc = nd.into_inner();
//!     Ok::<_, automerge::AutomergeError>(())
//! })?;
//! ```
//!
//! Document mutations (`with_doc`) are synchronous and microsecond-fast.
//! Only daemon protocol operations (`send_request`, `confirm_sync`) are async.
pub mod broadcast;
pub mod connect;
pub mod error;
pub mod handle;
mod shared;
mod snapshot;
pub mod sync_task;

pub use broadcast::BroadcastReceiver;
pub use error::SyncError;
pub use handle::DocHandle;
pub use shared::SharedDocState;
pub use snapshot::NotebookSnapshot;

#[cfg(test)]
mod tests;
