//! Frame type constants shared across all consumers (daemon, WASM, Python).
//!
//! These correspond to the first byte of each typed frame payload on
//! notebook sync connections. Defined here in `notebook-doc` so that
//! `runtimed` (daemon), `runtimed-wasm` (frontend), and `runtimed-py`
//! (Python bindings) all share one source of truth.
//!
//! The canonical `NotebookFrameType` enum with `TryFrom<u8>` lives in
//! `runtimed::connection` — these constants exist for consumers that
//! can't depend on `runtimed` (e.g., WASM).

/// Automerge sync message (binary).
pub const AUTOMERGE_SYNC: u8 = 0x00;

/// NotebookRequest (JSON).
pub const REQUEST: u8 = 0x01;

/// NotebookResponse (JSON).
pub const RESPONSE: u8 = 0x02;

/// NotebookBroadcast (JSON).
pub const BROADCAST: u8 = 0x03;

/// Presence (CBOR, see `presence` module).
pub const PRESENCE: u8 = 0x04;
