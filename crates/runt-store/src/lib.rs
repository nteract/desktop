//! `runt-store` — daemon local data store (pure-Rust `turso` spike).
//!
//! # What this is
//!
//! Third backend in the daemon-store evaluation after Lance (#2176)
//! and libSQL (#2178). This one uses the `turso` crate — the Turso
//! team's ground-up, pure-Rust rewrite of the SQLite engine.
//!
//! Same `TrustAllowlist` facade, same 9 integration tests, same
//! criterion harness. Apples-to-apples numbers.
//!
//! # Why pure Rust matters
//!
//! libSQL ships the SQLite C source via `libsql-ffi` + `cc`. That's
//! fine for macOS/Linux dev boxes but adds a C toolchain dependency
//! for every target, and complicates Windows cross-compilation. The
//! `turso` crate is 100% Rust — no `cc`, no `bindgen`, no C sources.
//! Worth paying some perf to keep the daemon's build story clean,
//! *if* the pure-Rust version is close enough.
//!
//! # Speed model
//!
//! Same in-memory-first pattern:
//!
//! - Reads on the hot path never touch disk. The whole table is
//!   materialized into a `HashSet` at `open()` time, `contains()`
//!   is a pure `HashSet` lookup.
//! - Writes update the set and append to the DB in the same call.
//!
//! # On disk
//!
//! Default root is `dirs::data_local_dir()/runt/store/`. One database
//! file: `allowlist.db`. No per-channel split.

pub mod paths;
pub mod trust_allowlist;

pub use paths::{default_store_dir, store_dir_for};
pub use trust_allowlist::{PackageManager, TrustAllowlist, TrustAllowlistError, TrustedPackage};
