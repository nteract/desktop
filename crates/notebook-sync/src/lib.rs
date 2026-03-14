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
//!   ├─ with_doc(|doc| doc.add_cell(...))   │
//!   │   → lock mutex                       │
//!   │   → mutate doc                       │
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
//! Document mutations (`with_doc`) are synchronous and microsecond-fast.
//! Only daemon protocol operations (`send_request`, `confirm_sync`) are async.

pub mod connect;
pub mod error;
pub mod handle;
mod shared;
mod snapshot;
pub mod sync_task;

pub use error::SyncError;
pub use handle::DocHandle;
pub use shared::SharedDocState;
pub use snapshot::NotebookSnapshot;
