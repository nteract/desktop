//! `runt-store` — daemon local data store (Turso/libSQL spike).
//!
//! # What this is
//!
//! Mirror of the LanceDB spike (PR #2176) using libSQL core only.
//! Same `TrustAllowlist` facade, same 9 integration tests, same
//! benchmark harness. Apples-to-apples comparison for the "what
//! should own daemon-side accumulated state?" decision.
//!
//! See PR #2176 for the LanceDB numbers and the broader workload
//! analysis (parquet/sift, Arrow IPC, indexed search, history cache).
//!
//! # Speed model
//!
//! Same in-memory-first pattern as the Lance spike:
//!
//! - Reads on the hot path never touch disk. The whole table is
//!   materialized into a `HashSet` at `open()` time, and `contains()`
//!   is a pure `HashSet` lookup.
//! - Writes update the set and append to the DB in the same call.
//!
//! libSQL gives us WAL + synchronous=NORMAL out of the box, which is
//! effectively what we'd want for an "append decisions, rarely remove"
//! workload.
//!
//! # On disk
//!
//! Default root is `dirs::data_local_dir()/runt/store/`. One database
//! file: `allowlist.db`. No per-channel split.

pub mod paths;
pub mod trust_allowlist;

pub use paths::{default_store_dir, store_dir_for};
pub use trust_allowlist::{PackageManager, TrustAllowlist, TrustAllowlistError, TrustedPackage};
