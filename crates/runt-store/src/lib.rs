//! `runt-store` — daemon local data store (spike).
//!
//! # What this is
//!
//! Scratch-space for evaluating [LanceDB](https://lancedb.com) as the
//! single persistent data store for the daemon's accumulated state:
//!
//! - Per-package trust allowlist (#2132) — the immediate driver.
//! - IPython execution history cache (future, for cross-session recall).
//! - Notebook / cell search (future, full-text + vector).
//!
//! The crate is currently unwired from `runtimed` on purpose. It lives
//! as a standalone library + benchmark harness so we can measure cold
//! startup, hot-path latency, write latency, and binary-size delta
//! before committing the rest of the daemon to LanceDB. If the numbers
//! disappoint, we swap the durability layer (redb, SQLite, JSON) without
//! touching call sites.
//!
//! # Speed model
//!
//! Reads on the hot path (e.g. "is `pandas` on the allowlist?", fired
//! every kernel launch) must not touch disk. Each store module loads
//! its whole table into an in-memory structure at daemon startup and
//! queries the in-memory view; writes update the in-memory view and
//! flush to LanceDB in the same call. This matches how
//! `room.trust_state.info` already works in `runtimed`.
//!
//! # Layout on disk
//!
//! Default root is `dirs::data_local_dir()/runt/store/`. A single Lance
//! *database* (directory of tables) lives there. Each workload gets its
//! own table. No per-channel split: stable and nightly share the same
//! accumulated user state, matching the `~/.config/runt/trust-key`
//! convention.

pub mod paths;
pub mod trust_allowlist;

pub use paths::{default_store_dir, store_dir_for};
pub use trust_allowlist::{PackageManager, TrustAllowlist, TrustAllowlistError, TrustedPackage};
