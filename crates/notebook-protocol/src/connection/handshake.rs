//! Connection handshake data structures.

use serde::{Deserialize, Serialize};

use super::env::PackageManager;

/// Channel handshake — the first frame on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Handshake {
    /// Pool IPC: environment take/return/status/ping.
    Pool,
    /// Automerge settings sync.
    SettingsSync,
    /// RPC settings channel: full snapshot push, scalar `SetSetting` writes.
    ///
    /// Opt-in alternative to `SettingsSync`. The daemon owns `settings.json`;
    /// clients send `SetSetting { key, value }` against the daemon's current
    /// snapshot rather than pushing CRDT state. Prototype for issue #1598.
    SettingsRpc,
    /// Automerge notebook sync (per-notebook room).
    ///
    /// The optional `protocol` field is accepted for version negotiation.
    /// v4 clients receive SessionControl frames. After handshake, the server
    /// sends a `ProtocolCapabilities` response before starting sync.
    NotebookSync {
        notebook_id: String,
        /// Protocol version requested by client (`v4`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<String>,
        /// Working directory for untitled notebooks (used for project file detection).
        /// When a notebook_id is a UUID (untitled), this provides the directory context
        /// for finding pyproject.toml, pixi.toml, or environment.yaml.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Serialized NotebookMetadataSnapshot JSON, sent with the initial handshake
        /// so the daemon can read kernelspec before auto-launching a kernel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_metadata: Option<String>,
    },
    /// Blob store: write blobs, query port.
    Blob,

    /// Open an existing notebook file. Daemon loads from disk, derives notebook_id.
    ///
    /// The daemon returns `NotebookConnectionInfo` before starting sync.
    /// After that, the connection becomes a normal notebook sync connection.
    OpenNotebook {
        /// Path to the .ipynb file.
        path: String,
    },

    /// Runtime agent handshake. Sent by the coordinator to a spawned runtime
    /// agent subprocess on its stdin. The runtime agent reads this, bootstraps
    /// its RuntimeStateDoc, and begins processing kernel requests.
    RuntimeAgent {
        /// Notebook room to attach to.
        notebook_id: String,
        /// Unique runtime agent identifier (e.g., "runtime-agent:a1b2c3d4").
        runtime_agent_id: String,
        /// Filesystem path to the shared blob store root
        /// (e.g., "~/.cache/runt/blobs/").
        blob_root: String,
    },

    /// Create a new untitled notebook. Daemon creates empty room, generates env_id.
    ///
    /// The daemon returns `NotebookConnectionInfo` before starting sync.
    /// After that, the connection becomes a normal notebook sync connection.
    CreateNotebook {
        /// Runtime type: "python" or "deno".
        runtime: String,
        /// Working directory for project file detection (pyproject.toml, pixi.toml, environment.yml).
        /// Used since untitled notebooks have no path to derive working_dir from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Optional notebook_id hint for restoring an untitled notebook from a previous session.
        /// If provided and the daemon has a persisted Automerge doc for this ID, the room is
        /// reused instead of creating a fresh empty notebook. If the persisted doc doesn't exist,
        /// a new notebook is created and this ID is used as the notebook_id/env_id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_id: Option<String>,
        /// When true, the notebook exists only in memory — no .automerge persisted to disk.
        /// Defaults to false (backward compat). MCP agents use true for scratch compute.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ephemeral: Option<bool>,
        /// Package manager preference: uv, conda, or pixi.
        /// When set, the daemon creates only this manager's metadata section.
        /// When None, the daemon uses its default_python_env setting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package_manager: Option<PackageManager>,
        /// Dependencies to seed into notebook metadata before auto-launch.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dependencies: Vec<String>,
    },
}

pub const PROTOCOL_V4: &str = "v4";

/// Numeric protocol version for version negotiation.
/// Increment this when making breaking protocol changes.
///
/// Protocol v4 removes legacy environment-sync request/response variants and
/// is not wire-compatible with v3 clients.
pub const PROTOCOL_VERSION: u32 = 4;

/// Server response indicating protocol capabilities.
///
/// Sent immediately after handshake, before starting sync.
/// Used by the `NotebookSync` handshake variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    /// Protocol version string (currently always "v4").
    pub protocol: String,
    /// Numeric protocol version for explicit version checking.
    /// Clients can compare this against their expected version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    /// Useful for debugging version mismatches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
}

/// Server response for `OpenNotebook` and `CreateNotebook` handshakes.
///
/// Sent immediately after handshake, before starting sync.
/// Contains notebook_id derived by the daemon (from path or generated env_id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookConnectionInfo {
    /// Protocol version string (currently always "v4").
    pub protocol: String,
    /// Numeric protocol version for explicit version checking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    /// Notebook identifier derived by the daemon.
    /// For existing files: canonical path.
    /// For new notebooks: generated UUID (env_id).
    pub notebook_id: String,
    /// Number of cells in the notebook (for progress indication).
    pub cell_count: usize,
    /// True if the notebook has untrusted dependencies requiring user approval.
    pub needs_trust_approval: bool,
    /// Error message if the notebook could not be opened/created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this notebook is ephemeral (in-memory only, no persistence).
    #[serde(default)]
    pub ephemeral: bool,
    /// On-disk path when the room is file-backed. Populated by `CreateNotebook`
    /// when `notebook_id_hint` resolves to a room that already has a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notebook_path: Option<String>,
}
