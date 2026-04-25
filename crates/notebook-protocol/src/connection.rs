//! Unified connection framing and handshake for the runtimed socket.
//!
//! Every connection begins with a 5-byte preamble:
//!
//! ```text
//! [4 bytes: magic 0xC0DE01AC] [1 byte: protocol version]
//! ```
//!
//! Followed by length-prefixed binary framing:
//!
//! ```text
//! [4 bytes: payload length (big-endian u32)] [payload bytes]
//! ```
//!
//! The first frame after the preamble is a JSON handshake declaring the
//! channel. After the handshake, the daemon routes the connection to the
//! appropriate handler.
//!
//! ## Notebook Sync Frame Types (Phase 8)
//!
//! Notebook sync connections use a typed frame format where the first byte
//! of the payload indicates the frame type:
//!
//! - `0x00`: Automerge sync message (binary)
//! - `0x01`: NotebookRequest (JSON)
//! - `0x02`: NotebookResponse (JSON)
//! - `0x03`: NotebookBroadcast (JSON)
//! - `0x07`: SessionControl (JSON, server-originated)

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Supported package managers for Python notebooks.
///
/// The canonical wire format is the lowercase variant name (`"uv"`, `"conda"`,
/// `"pixi"`). `parse()` additionally accepts `"pip"` (→ Uv) and `"mamba"` (→
/// Conda) as aliases at user-input boundaries.
///
/// Deserialization is permissive: unrecognized wire strings land in
/// `Unknown(s)` rather than failing, so the `CreateNotebook` handshake stays
/// forward-compatible and legacy aliases still decode. Resolve `Unknown`
/// values to a canonical variant at use-site via `resolve()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
    /// An unrecognized package manager string, carried verbatim from the wire.
    ///
    /// Produced only by `Deserialize` for non-canonical values; internal code
    /// should call `resolve()` before matching. User-input boundaries (Python
    /// API, MCP `create_notebook`, CLI) should call `parse()` instead, which
    /// rejects unknowns up front.
    Unknown(String),
}

impl PackageManager {
    /// The wire string form.
    ///
    /// Canonical variants return their literal name; `Unknown(s)` returns the
    /// raw string that was deserialized.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Uv => "uv",
            Self::Conda => "conda",
            Self::Pixi => "pixi",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// Parse a package manager name with alias support.
    ///
    /// Accepts `"uv"`, `"conda"`, `"pixi"` (canonical), plus `"pip"` (→ Uv)
    /// and `"mamba"` (→ Conda). Returns `Err` for anything else.
    ///
    /// Use this at user-input boundaries where immediate validation is
    /// desired. Wire deserialization is permissive and never errors — see
    /// `Unknown`.
    pub fn parse(input: &str) -> Result<Self, String> {
        match input {
            "uv" => Ok(Self::Uv),
            "conda" => Ok(Self::Conda),
            "pixi" => Ok(Self::Pixi),
            "pip" => Ok(Self::Uv),
            "mamba" => Ok(Self::Conda),
            _ => Err(format!(
                "Unsupported package manager '{}'. Supported: uv, conda, pixi.",
                input
            )),
        }
    }

    /// Fold to a canonical variant, resolving known aliases.
    ///
    /// `Uv`/`Conda`/`Pixi` pass through. `Unknown("pip")` → `Uv`,
    /// `Unknown("mamba")` → `Conda`. Any other `Unknown(s)` returns `Err`.
    ///
    /// Call this at internal evaluation sites where the code needs one of the
    /// three canonical variants. Error handling is up to the caller — the
    /// daemon handshake path falls back to `default_python_env`, for example.
    pub fn resolve(&self) -> Result<Self, String> {
        match self {
            Self::Uv => Ok(Self::Uv),
            Self::Conda => Ok(Self::Conda),
            Self::Pixi => Ok(Self::Pixi),
            Self::Unknown(s) => Self::parse(s),
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PackageManager {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for PackageManager {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PackageManager {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        // Permissive: unknown strings are captured rather than rejected, so
        // the handshake stays forward-compatible and legacy aliases
        // (`"pip"`, `"mamba"`) still decode. Internal callers fold aliases
        // and reject genuine unknowns via `resolve()`.
        if raw == "uv" {
            Ok(Self::Uv)
        } else if raw == "conda" {
            Ok(Self::Conda)
        } else if raw == "pixi" {
            Ok(Self::Pixi)
        } else {
            Ok(Self::Unknown(raw))
        }
    }
}

/// A concrete, resolved environment source.
///
/// Carried on `KernelLaunched.env_source` and
/// `RuntimeStateDoc.kernel.env_source`. The daemon resolves the request-time
/// `LaunchSpec` into an `EnvSource` before routing the launch; every
/// downstream code path works against this type.
///
/// Deserialization is permissive: unrecognized wire strings land in
/// `Unknown(s)` rather than failing, so the daemon and clients stay
/// forward-compatible across versions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnvSource {
    /// Prewarmed pool env (e.g. `"uv:prewarmed"`). The daemon acquires these
    /// from its warming pool — they do not prepare their own env.
    Prewarmed(PackageManager),
    /// Dependencies declared in notebook metadata (e.g. `"uv:inline"`). The
    /// daemon builds the env from the metadata before kernel launch.
    Inline(PackageManager),
    /// `pyproject.toml` on disk (`"uv:pyproject"`). UV-only by definition.
    Pyproject,
    /// `pixi.toml` on disk (`"pixi:toml"`). Pixi-only.
    PixiToml,
    /// `environment.yml` on disk (`"conda:env_yml"`). Conda-only.
    EnvYml,
    /// PEP 723 script deps extracted from cell source (e.g. `"uv:pep723"`).
    Pep723(PackageManager),
    /// Deno TypeScript kernel — no Python env.
    Deno,
    /// Unrecognized wire string, preserved verbatim. Produced only by
    /// `Deserialize` for values we haven't taught the enum about. Handle this
    /// at match sites by falling back to the historical default (usually
    /// Uv-family behavior) — never panic.
    Unknown(String),
}

impl EnvSource {
    /// The wire string form.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Prewarmed(PackageManager::Uv) => "uv:prewarmed",
            Self::Prewarmed(PackageManager::Conda) => "conda:prewarmed",
            Self::Prewarmed(PackageManager::Pixi) => "pixi:prewarmed",
            Self::Prewarmed(PackageManager::Unknown(s)) => s.as_str(),
            Self::Inline(PackageManager::Uv) => "uv:inline",
            Self::Inline(PackageManager::Conda) => "conda:inline",
            Self::Inline(PackageManager::Pixi) => "pixi:inline",
            Self::Inline(PackageManager::Unknown(s)) => s.as_str(),
            Self::Pyproject => "uv:pyproject",
            Self::PixiToml => "pixi:toml",
            Self::EnvYml => "conda:env_yml",
            Self::Pep723(PackageManager::Uv) => "uv:pep723",
            Self::Pep723(PackageManager::Conda) => "conda:pep723",
            Self::Pep723(PackageManager::Pixi) => "pixi:pep723",
            Self::Pep723(PackageManager::Unknown(s)) => s.as_str(),
            Self::Deno => "deno",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// Parse a wire string. Never fails — unrecognized values land in
    /// `Unknown(s)`.
    pub fn parse(input: &str) -> Self {
        match input {
            "uv:prewarmed" => Self::Prewarmed(PackageManager::Uv),
            "conda:prewarmed" => Self::Prewarmed(PackageManager::Conda),
            "pixi:prewarmed" => Self::Prewarmed(PackageManager::Pixi),
            "uv:inline" => Self::Inline(PackageManager::Uv),
            "conda:inline" => Self::Inline(PackageManager::Conda),
            "pixi:inline" => Self::Inline(PackageManager::Pixi),
            "uv:pyproject" => Self::Pyproject,
            "pixi:toml" => Self::PixiToml,
            "conda:env_yml" => Self::EnvYml,
            "uv:pep723" => Self::Pep723(PackageManager::Uv),
            "conda:pep723" => Self::Pep723(PackageManager::Conda),
            "pixi:pep723" => Self::Pep723(PackageManager::Pixi),
            "deno" => Self::Deno,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The package manager associated with this env source, if any.
    ///
    /// For canonical variants this returns the associated manager directly.
    /// For `Unknown(s)`, the wire string's prefix is inspected so that a
    /// forward-compatible value like `"conda:foo"` or `"pixi:bar"` still
    /// routes to the correct package manager family. `Deno` and `Unknown`
    /// strings without a recognized prefix return `None`.
    pub fn package_manager(&self) -> Option<PackageManager> {
        match self {
            Self::Prewarmed(pm) | Self::Inline(pm) | Self::Pep723(pm) => Some(pm.clone()),
            Self::Pyproject => Some(PackageManager::Uv),
            Self::PixiToml => Some(PackageManager::Pixi),
            Self::EnvYml => Some(PackageManager::Conda),
            Self::Deno => None,
            Self::Unknown(s) => {
                if s.starts_with("uv:") {
                    Some(PackageManager::Uv)
                } else if s.starts_with("conda:") {
                    Some(PackageManager::Conda)
                } else if s.starts_with("pixi:") {
                    Some(PackageManager::Pixi)
                } else {
                    None
                }
            }
        }
    }

    /// True if this source prepares its own environment (no pool env needed).
    ///
    /// Used at auto-launch time to decide whether to acquire a prewarmed env
    /// from the pool. `Inline`, project-file, and `Pep723` sources build
    /// their env themselves; `Prewarmed` pulls from the pool; `Deno` and
    /// `Unknown` take the no-pool path.
    pub fn prepares_own_env(&self) -> bool {
        matches!(
            self,
            Self::Inline(_) | Self::Pyproject | Self::PixiToml | Self::EnvYml | Self::Pep723(_)
        )
    }
}

impl fmt::Display for EnvSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EnvSource {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EnvSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::parse(&raw))
    }
}

/// Request-time launch specification.
///
/// The caller of `LaunchKernel` sends a `LaunchSpec`. The daemon resolves
/// it into a concrete `EnvSource` before routing the launch. This type
/// keeps the auto-detection inputs (`""`, `"auto"`, `"auto:uv"`,
/// `"prewarmed"`) visibly distinct from a concrete env_source in the type
/// system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSpec {
    /// Auto-detect everything — derived from notebook metadata, project
    /// files, or PEP 723 script blocks. Wire strings: `""`, `"auto"`,
    /// `"prewarmed"` (legacy alias).
    Auto,
    /// Auto-detect within a specific package manager family. Wire strings:
    /// `"auto:uv"`, `"auto:conda"`, `"auto:pixi"`.
    AutoScoped(PackageManager),
    /// A concrete env_source to honor as-is.
    Concrete(EnvSource),
}

impl LaunchSpec {
    /// Parse a launch spec from the wire string.
    pub fn parse(input: &str) -> Self {
        match input {
            "" | "auto" | "prewarmed" => Self::Auto,
            "auto:uv" => Self::AutoScoped(PackageManager::Uv),
            "auto:conda" => Self::AutoScoped(PackageManager::Conda),
            "auto:pixi" => Self::AutoScoped(PackageManager::Pixi),
            other => Self::Concrete(EnvSource::parse(other)),
        }
    }

    /// If this spec is `AutoScoped(pm)`, returns `Some(pm)`; otherwise None.
    pub fn auto_scope(&self) -> Option<PackageManager> {
        match self {
            Self::AutoScoped(pm) => Some(pm.clone()),
            _ => None,
        }
    }
}

// ── Frame size limits ───────────────────────────────────────────────
//
// Every typed frame has a hard cap and a soft warn threshold. The cap
// rejects the frame outright (drops the connection); the warn threshold
// just logs so we see growth before a cap rejection is triggered in
// production.
//
// Caps are sized for *legitimate worst case*, not the typical case.
// They exist as defense-in-depth against:
//   1. parser/serde bugs that might let a giant payload land on the
//      wrong channel (e.g. kernel output bytes confused for a Request),
//   2. runaway senders OOMing the receiver,
//   3. corrupted length prefixes on the wire.
//
// They do not replace per-content offloading — large outputs and widget
// buffers still belong in the blob store.

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// Outer ceiling for any frame. Matches the blob-store size limit and
/// the u32 length-prefix domain. Per-type caps below are tighter for
/// every channel.
const MAX_FRAME_SIZE: usize = 100 * MIB;

/// Maximum frame size for control/handshake frames: 64 KiB.
/// Applied to the initial handshake (before per-type routing exists).
const MAX_CONTROL_FRAME_SIZE: usize = 64 * KIB;

/// Per-type body cap and warn threshold for a typed frame.
#[derive(Debug, Clone, Copy)]
struct FrameSizeLimits {
    /// Hard cap. Frames exceeding this are rejected.
    cap: usize,
    /// Soft threshold. Frames exceeding this log a warning but proceed.
    warn: usize,
}

/// Per-type body limits.
///
/// Sized for legitimate worst case per channel:
/// - **AutomergeSync** (NotebookDoc) and **RuntimeStateSync**: 64 MiB.
///   Initial sync of a notebook with many cells / lots of accumulated
///   ephemeral state can be many MB; outputs are blob-offloaded but
///   the doc itself can still be large.
/// - **NotebookRequest**: 16 MiB. Most variants are tiny (cell IDs,
///   metadata snapshots, completion requests) — they sit well under the
///   256 KiB warn so growth on them still surfaces. The cap exists for
///   one outlier: `SendComm` carries the same comm envelope shape as
///   `NotebookBroadcast::Comm`, including custom-message buffers from
///   `model.send(content, callbacks, buffers)`. JSON-encoding `Vec<Vec<u8>>`
///   inflates binary by ~4×, so a 256 KiB widget buffer becomes ~1 MiB
///   on the wire. Tightens once `SendComm.buffers` and
///   `NotebookBroadcast::Comm.buffers` both move through the blob store.
/// - **NotebookResponse**: 64 MiB. `Response::DocBytes` returns a full
///   Automerge doc dump; `Response::NotebookState` carries
///   inspect-notebook output. Both can legitimately be large.
/// - **NotebookBroadcast**: 16 MiB. `Comm.buffers` is now `Vec<BlobRef>`
///   (kernel-side runtime agent puts buffers in the blob store before
///   broadcasting). The cap stays loose in this revision pending a
///   follow-up that drops it to 1 MiB along with the matching tightening
///   on Request once `SendComm.buffers` is also offloaded.
/// - **Presence**: 1 MiB. CBOR cursor + selection per peer.
/// - **PoolStateSync**: 1 MiB. The pool doc is bounded.
/// - **SessionControl**: 1 MiB. JSON readiness frames are tiny.
fn frame_size_limits(type_byte: u8) -> FrameSizeLimits {
    use notebook_doc::frame_types;
    match type_byte {
        frame_types::AUTOMERGE_SYNC => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::REQUEST => FrameSizeLimits {
            cap: 16 * MIB,
            warn: 256 * KIB,
        },
        frame_types::RESPONSE => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::BROADCAST => FrameSizeLimits {
            cap: 16 * MIB,
            warn: 4 * MIB,
        },
        frame_types::PRESENCE => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        frame_types::RUNTIME_STATE_SYNC => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::POOL_STATE_SYNC => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        frame_types::SESSION_CONTROL => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        // Unknown types fall back to the outer ceiling. The receive
        // path skips them anyway after the body is consumed.
        _ => FrameSizeLimits {
            cap: MAX_FRAME_SIZE,
            warn: MAX_FRAME_SIZE / 2,
        },
    }
}

/// Channel handshake — the first frame on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Handshake {
    /// Pool IPC: environment take/return/status/ping.
    Pool,
    /// Automerge settings sync.
    SettingsSync,
    /// Automerge notebook sync (per-notebook room).
    ///
    /// The optional `protocol` field is accepted for future version negotiation.
    /// Currently only v2 (typed frames) is supported. After handshake, the server
    /// sends a `ProtocolCapabilities` response before starting sync.
    NotebookSync {
        notebook_id: String,
        /// Protocol version requested by client. Currently only v2 is supported.
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

/// Protocol version constants (strings for handshake compatibility).
pub const PROTOCOL_V2: &str = "v2";
pub const PROTOCOL_V3: &str = "v3";

/// Numeric protocol version for version negotiation.
/// Increment this when making breaking protocol changes.
///
/// Protocol v3 adds explicit session-control status frames for notebook
/// bootstrap/readiness and is not wire-compatible with v2 clients.
pub const PROTOCOL_VERSION: u32 = 3;

/// Minimum protocol version accepted by v3 daemons for backward compatibility.
/// v2 clients are served without SESSION_CONTROL frames.
pub const MIN_PROTOCOL_VERSION: u32 = 2;

/// Magic bytes identifying the runtimed protocol.
/// Sent as the first 4 bytes of every connection, before the handshake frame.
pub const MAGIC: [u8; 4] = [0xC0, 0xDE, 0x01, 0xAC];

/// Total preamble size: 4-byte magic + 1-byte protocol version.
pub const PREAMBLE_LEN: usize = 5;

/// Send the connection preamble (magic bytes + protocol version).
///
/// Must be called once at the start of every connection, before
/// the handshake frame.
pub async fn send_preamble<W: AsyncWrite + Unpin>(writer: &mut W) -> std::io::Result<()> {
    const _: () = assert!(
        PROTOCOL_VERSION <= u8::MAX as u32,
        "PROTOCOL_VERSION must fit in a single byte for the wire preamble"
    );
    let mut buf = [0u8; PREAMBLE_LEN];
    buf[..4].copy_from_slice(&MAGIC);
    buf[4] = PROTOCOL_VERSION as u8;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive and validate the connection preamble.
///
/// Returns the protocol version byte. Returns an error if the magic bytes
/// don't match or the protocol version is incompatible.
pub async fn recv_preamble<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<u8> {
    let mut buf = [0u8; PREAMBLE_LEN];
    match reader.read_exact(&mut buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before preamble",
            ));
        }
        Err(e) => return Err(e),
    }

    if buf[..4] != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid magic bytes: expected {:02X?}, got {:02X?}",
                MAGIC,
                &buf[..4]
            ),
        ));
    }

    let version = buf[4];
    if version != PROTOCOL_VERSION as u8 {
        let direction = if (version as u32) > PROTOCOL_VERSION {
            "The daemon is newer than this client. Please update the CLI (or reinstall the app)."
        } else {
            "The daemon is older than this client. Please update the daemon: runt daemon doctor --fix"
        };
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "protocol version mismatch: daemon has v{}, client expects v{}. {}",
                version, PROTOCOL_VERSION, direction
            ),
        ));
    }

    Ok(version)
}

/// Server response indicating protocol capabilities.
///
/// Sent immediately after handshake, before starting sync.
/// Used by the `NotebookSync` handshake variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    /// Protocol version string (currently always "v3").
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
    /// Protocol version string (currently always "v3").
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

/// Frame types for notebook sync connections.
///
/// The first byte of each frame payload indicates the type of message.
/// The byte values are defined in `notebook_doc::frame_types` so all
/// consumers (daemon, WASM, Python) share one source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotebookFrameType {
    /// Automerge sync message (binary).
    AutomergeSync = notebook_doc::frame_types::AUTOMERGE_SYNC,
    /// NotebookRequest (JSON).
    Request = notebook_doc::frame_types::REQUEST,
    /// NotebookResponse (JSON).
    Response = notebook_doc::frame_types::RESPONSE,
    /// NotebookBroadcast (JSON).
    Broadcast = notebook_doc::frame_types::BROADCAST,
    /// Presence (CBOR, see notebook_doc::presence).
    Presence = notebook_doc::frame_types::PRESENCE,
    /// RuntimeStateDoc sync message (binary Automerge sync).
    RuntimeStateSync = notebook_doc::frame_types::RUNTIME_STATE_SYNC,
    /// PoolDoc sync message (binary Automerge sync, global).
    PoolStateSync = notebook_doc::frame_types::POOL_STATE_SYNC,
    /// Session-control message (JSON, server-originated connection status).
    SessionControl = notebook_doc::frame_types::SESSION_CONTROL,
}

impl TryFrom<u8> for NotebookFrameType {
    type Error = std::io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use notebook_doc::frame_types;
        match value {
            frame_types::AUTOMERGE_SYNC => Ok(Self::AutomergeSync),
            frame_types::REQUEST => Ok(Self::Request),
            frame_types::RESPONSE => Ok(Self::Response),
            frame_types::BROADCAST => Ok(Self::Broadcast),
            frame_types::PRESENCE => Ok(Self::Presence),
            frame_types::RUNTIME_STATE_SYNC => Ok(Self::RuntimeStateSync),
            frame_types::POOL_STATE_SYNC => Ok(Self::PoolStateSync),
            frame_types::SESSION_CONTROL => Ok(Self::SessionControl),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown notebook frame type: 0x{:02x}", value),
            )),
        }
    }
}

/// A typed notebook frame with its type and payload.
#[derive(Debug)]
pub struct TypedNotebookFrame {
    pub frame_type: NotebookFrameType,
    pub payload: Vec<u8>,
}

/// Send a typed notebook frame.
///
/// Enforces the same per-type cap the receiver applies, so an outbound
/// oversize is caught with a clear local error rather than a generic
/// `frame too large` from the peer. A soft warn fires between the warn
/// threshold and the cap so we see growth before it ever rejects.
pub async fn send_typed_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame_type: NotebookFrameType,
    payload: &[u8],
) -> std::io::Result<()> {
    let type_byte = frame_type as u8;
    let limits = frame_size_limits(type_byte);
    if payload.len() > limits.cap {
        log::error!(
            "[notebook-protocol] outbound frame type 0x{:02x} exceeds cap: {} bytes (cap {})",
            type_byte,
            payload.len(),
            limits.cap,
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "outbound frame too large for type 0x{:02x}: {} bytes (max {})",
                type_byte,
                payload.len(),
                limits.cap
            ),
        ));
    }
    if payload.len() > limits.warn {
        log::warn!(
            "[notebook-protocol] outbound frame type 0x{:02x} over warn threshold: {} bytes (warn {}, cap {})",
            type_byte,
            payload.len(),
            limits.warn,
            limits.cap,
        );
    }
    let mut data = Vec::with_capacity(1 + payload.len());
    data.push(type_byte);
    data.extend_from_slice(payload);
    send_frame(writer, &data).await
}

/// Send a typed notebook frame with JSON payload.
pub async fn send_typed_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    frame_type: NotebookFrameType,
    value: &T,
) -> anyhow::Result<()> {
    let json_bytes = serde_json::to_vec(value)?;
    send_typed_frame(writer, frame_type, &json_bytes).await?;
    Ok(())
}

/// Receive a typed notebook frame.
/// Returns `None` on clean disconnect (EOF).
/// Unknown frame types are logged and skipped for forward compatibility.
///
/// Length is read first, then the 1-byte type discriminator, then the
/// per-type cap is applied before the body is read. This means a
/// garbage length prefix aimed at, say, the `Request` channel (e.g.
/// 1.8 GB) is rejected before the allocator tries to honor it.
pub async fn recv_typed_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<TypedNotebookFrame>> {
    loop {
        // Read the 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty frame",
            ));
        }
        // Outer ceiling before we even look at the type byte — 100 MiB.
        if len > MAX_FRAME_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame too large: {} bytes (max {})", len, MAX_FRAME_SIZE),
            ));
        }

        // Read the 1-byte type discriminator.
        let mut type_buf = [0u8; 1];
        reader.read_exact(&mut type_buf).await?;
        let type_byte = type_buf[0];
        let body_len = len - 1;

        // Per-type ceiling. The hard cap rejects oversized payloads —
        // a corrupted length prefix on a narrow-purpose channel trips
        // this check before the allocator honors the bogus length. The
        // soft warn threshold logs growth so we see drift in production
        // before it ever rejects.
        let limits = frame_size_limits(type_byte);
        if body_len > limits.cap {
            log::error!(
                "[notebook-protocol] frame type 0x{:02x} exceeds cap: {} bytes (cap {}); dropping connection",
                type_byte,
                body_len,
                limits.cap,
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "frame too large for type 0x{:02x}: {} bytes (max {})",
                    type_byte, body_len, limits.cap
                ),
            ));
        }
        if body_len > limits.warn {
            log::warn!(
                "[notebook-protocol] frame type 0x{:02x} over warn threshold: {} bytes (warn {}, cap {})",
                type_byte,
                body_len,
                limits.warn,
                limits.cap,
            );
        }

        // Now it's safe to allocate and read the body.
        let mut payload = vec![0u8; body_len];
        reader.read_exact(&mut payload).await?;

        match NotebookFrameType::try_from(type_byte) {
            Ok(frame_type) => {
                return Ok(Some(TypedNotebookFrame {
                    frame_type,
                    payload,
                }));
            }
            Err(_) => {
                log::warn!(
                    "Skipping unknown notebook frame type 0x{:02x} ({} bytes payload)",
                    type_byte,
                    body_len,
                );
                continue;
            }
        }
    }
}

/// Cancel-safe wrapper around `recv_typed_frame`.
///
/// `recv_typed_frame` is built on `read_exact`, which is *not*
/// cancel-safe — dropping the future mid-read silently consumes bytes
/// from the underlying reader and the partial-read state is discarded.
/// Putting `recv_typed_frame` directly inside a busy `tokio::select!`
/// arm therefore desyncs the stream the moment another arm wins while
/// a body read is in flight; the next iteration's length prefix is
/// read from the middle of the previous payload.
///
/// `FramedReader` runs the read loop on a dedicated tokio task that
/// owns the read half exclusively and publishes frames through a
/// bounded mpsc channel. `FramedReader::recv()` is just an mpsc
/// `recv()` — fully cancel-safe — so callers can place it in any
/// `select!` without losing bytes.
///
/// On clean EOF the channel closes and `recv()` returns `None`.
/// On a stream error the reader sends `Err(e)` once and then closes.
pub struct FramedReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<TypedNotebookFrame>>,
    handle: tokio::task::JoinHandle<()>,
}

impl FramedReader {
    /// Spawn a reader task that owns `reader`. `capacity` bounds the
    /// in-flight frame queue so a slow consumer applies backpressure
    /// to the source.
    pub fn spawn<R>(mut reader: R, capacity: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let handle = tokio::spawn(async move {
            loop {
                match recv_typed_frame(&mut reader).await {
                    Ok(Some(frame)) => {
                        if tx.send(Ok(frame)).await.is_err() {
                            // Receiver dropped — caller is gone, stop reading.
                            break;
                        }
                    }
                    Ok(None) => break, // clean EOF, drop tx, channel closes
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
        });
        Self { rx, handle }
    }

    /// Cancel-safe receive of the next frame.
    ///
    /// Returns `Some(Ok(frame))` for each successful frame,
    /// `Some(Err(_))` once on a stream error, and `None` once the
    /// reader task has finished (clean EOF or post-error close).
    pub async fn recv(&mut self) -> Option<std::io::Result<TypedNotebookFrame>> {
        self.rx.recv().await
    }
}

impl Drop for FramedReader {
    fn drop(&mut self) {
        // Closing the receiver lets the task observe a closed channel
        // on its next send and exit cleanly. Abort is a backstop for
        // the case where the task is parked on `read_exact` with no
        // bytes ever arriving (e.g. half-closed socket).
        self.handle.abort();
    }
}

/// Send a length-prefixed frame.
///
/// Returns an error if the payload exceeds `MAX_FRAME_SIZE` (100 MiB).
/// This prevents silent truncation of the 4-byte length field at the u32
/// boundary and keeps send/receive limits symmetric.
pub async fn send_frame<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    if data.len() > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "frame too large to send: {} bytes (max {})",
                data.len(),
                MAX_FRAME_SIZE
            ),
        ));
    }
    let len = (data.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive a length-prefixed frame with a caller-specified size limit.
/// Returns `None` on clean disconnect (EOF).
async fn recv_frame_with_limit<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_size: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > max_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {} bytes (max {})", len, max_size),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Receive a length-prefixed frame (up to 100 MiB for data payloads).
/// Returns `None` on clean disconnect (EOF).
pub async fn recv_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    recv_frame_with_limit(reader, MAX_FRAME_SIZE).await
}

/// Receive a length-prefixed frame with the control/handshake size limit
/// (64 KiB). Use this for handshake and JSON request/response traffic to
/// prevent oversized frames from forcing large allocations.
pub async fn recv_control_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    recv_frame_with_limit(reader, MAX_CONTROL_FRAME_SIZE).await
}

/// Send a value as a JSON-encoded length-prefixed frame.
pub async fn send_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(value)?;
    send_frame(writer, &data).await?;
    Ok(())
}

/// Receive and deserialize a JSON-encoded length-prefixed frame.
/// Returns `None` on clean disconnect (EOF).
pub async fn recv_json_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> anyhow::Result<Option<T>> {
    match recv_frame(reader).await? {
        Some(data) => {
            let value = serde_json::from_slice(&data)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_frame_roundtrip() {
        let data = b"hello world";

        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();
        assert_eq!(buf.len(), 4 + data.len());

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_frame_eof() {
        let buf: &[u8] = &[];
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_frame(&mut cursor).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_frame_too_large_recv() {
        let len_bytes = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(len_bytes.to_vec());
        let result = recv_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_frame_too_large_send() {
        let data = vec![0u8; MAX_FRAME_SIZE + 1];
        let mut buf = Vec::new();
        let result = send_frame(&mut buf, &data).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn test_control_frame_rejects_oversized() {
        // A frame larger than 64 KiB should be rejected by recv_control_frame
        let oversized_len = (MAX_CONTROL_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(oversized_len.to_vec());
        let result = recv_control_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_control_frame_accepts_small() {
        let data = b"small control payload";
        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_control_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_json_frame_roundtrip() {
        let handshake = Handshake::Pool;

        let mut buf = Vec::new();
        send_json_frame(&mut buf, &handshake).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received: Handshake = recv_json_frame(&mut cursor).await.unwrap().unwrap();
        assert!(matches!(received, Handshake::Pool));
    }

    #[tokio::test]
    async fn test_preamble_roundtrip() {
        let mut buf = Vec::new();
        send_preamble(&mut buf).await.unwrap();
        assert_eq!(buf.len(), PREAMBLE_LEN);
        assert_eq!(&buf[..4], &MAGIC);
        assert_eq!(buf[4], PROTOCOL_VERSION as u8);

        let mut cursor = std::io::Cursor::new(buf);
        let version = recv_preamble(&mut cursor).await.unwrap();
        assert_eq!(version, PROTOCOL_VERSION as u8);
    }

    #[tokio::test]
    async fn test_preamble_bad_magic() {
        let buf = [0xFF, 0xFF, 0xFF, 0xFF, PROTOCOL_VERSION as u8];
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_version_mismatch() {
        let mut buf = [0u8; PREAMBLE_LEN];
        buf[..4].copy_from_slice(&MAGIC);
        buf[4] = 99; // wrong version
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_eof() {
        let buf: &[u8] = &[0xC0, 0xDE]; // incomplete
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn test_handshake_serialization() {
        // Pool
        let json = serde_json::to_string(&Handshake::Pool).unwrap();
        assert_eq!(json, r#"{"channel":"pool"}"#);

        // SettingsSync
        let json = serde_json::to_string(&Handshake::SettingsSync).unwrap();
        assert_eq!(json, r#"{"channel":"settings_sync"}"#);

        // NotebookSync (without protocol - should omit the field)
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: None,
            working_dir: None,
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"notebook_sync","notebook_id":"abc"}"#);

        // NotebookSync with v2 protocol
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: Some("v2".into()),
            working_dir: None,
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"abc","protocol":"v2"}"#
        );

        // NotebookSync with working_dir for untitled notebook
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            protocol: Some("v2".into()),
            working_dir: Some("/home/user/project".into()),
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"550e8400-e29b-41d4-a716-446655440000","protocol":"v2","working_dir":"/home/user/project"}"#
        );

        // Blob
        let json = serde_json::to_string(&Handshake::Blob).unwrap();
        assert_eq!(json, r#"{"channel":"blob"}"#);

        // OpenNotebook
        let json = serde_json::to_string(&Handshake::OpenNotebook {
            path: "/home/user/notebook.ipynb".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"open_notebook","path":"/home/user/notebook.ipynb"}"#
        );

        // CreateNotebook without working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: None,
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"create_notebook","runtime":"python"}"#);

        // CreateNotebook with working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "deno".into(),
            working_dir: Some("/home/user/project".into()),
            notebook_id: None,
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"deno","working_dir":"/home/user/project"}"#
        );

        // CreateNotebook with notebook_id hint (session restore)
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"python","notebook_id":"550e8400-e29b-41d4-a716-446655440000"}"#
        );
    }

    #[test]
    fn test_notebook_connection_info_serialization() {
        // Success case (minimal - no optional fields)
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(
            json,
            r#"{"protocol":"v2","notebook_id":"/home/user/notebook.ipynb","cell_count":5,"needs_trust_approval":false,"ephemeral":false}"#
        );

        // With version info
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V3.into(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some("0.1.0+abc123".into()),
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(&format!(r#""protocol_version":{}"#, PROTOCOL_VERSION)));
        assert!(json.contains(r#""daemon_version":"0.1.0+abc123""#));

        // With trust approval needed
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            cell_count: 1,
            needs_trust_approval: true,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""needs_trust_approval":true"#));

        // Error case
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: String::new(),
            cell_count: 0,
            needs_trust_approval: false,
            error: Some("File not found".into()),
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""error":"File not found""#));

        // With notebook_path
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: Some("/home/user/notebook.ipynb".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""notebook_path":"/home/user/notebook.ipynb""#));

        // Backward compat: deserialize without notebook_path
        let old_json = r#"{"protocol":"v2","notebook_id":"abc","cell_count":1,"needs_trust_approval":false,"ephemeral":false}"#;
        let info: NotebookConnectionInfo = serde_json::from_str(old_json).unwrap();
        assert!(info.notebook_path.is_none());
    }

    #[tokio::test]
    async fn typed_frame_rejects_oversized_presence() {
        // Presence frames cap at 1 MiB. A desync that happens to land
        // on the Presence channel with a multi-MiB length header is
        // caught here instead of trying to allocate it.
        let cap = frame_size_limits(notebook_doc::frame_types::PRESENCE).cap;
        let body_len: u32 = (cap as u32) + 1;
        let total_len: u32 = body_len + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(notebook_doc::frame_types::PRESENCE);
        let mut cursor = std::io::Cursor::new(buf);
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("too large for type 0x04"));
    }

    #[tokio::test]
    async fn typed_frame_allows_big_broadcast_for_widget_comm() {
        // `NotebookBroadcast::Comm` carries widget envelopes with inline
        // binary buffers. The Broadcast cap (16 MiB) leaves room for
        // legitimate widget messages while still being far below the
        // outer ceiling.
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Broadcast, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Broadcast);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_rejects_oversized_request() {
        // The Request cap rejects payloads that exceed the channel's
        // legitimate worst case (today: a SendComm envelope with widget
        // buffers that JSON-expand from binary).
        let cap = frame_size_limits(notebook_doc::frame_types::REQUEST).cap;
        let body_len: u32 = (cap as u32) + 1;
        let total_len: u32 = body_len + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(notebook_doc::frame_types::REQUEST);
        let mut cursor = std::io::Cursor::new(buf);
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("too large for type 0x01"));
    }

    #[tokio::test]
    async fn typed_frame_allows_sendcomm_with_widget_buffers() {
        // Custom comm messages from `model.send(content, callbacks, buffers)`
        // ride `NotebookRequest::SendComm`. JSON-encoding `Vec<Vec<u8>>`
        // expands binary by ~4×, so a 256 KiB widget buffer becomes
        // ~1 MiB on the wire. The Request cap must accommodate this.
        // 4 MiB simulates a buffer roughly equivalent to a 1 MiB binary
        // payload after JSON expansion — a realistic moderate-size
        // custom widget message.
        let big_payload = vec![0x42u8; 4 * 1024 * 1024];
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Request, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_send_rejects_outbound_oversize() {
        // The send path mirrors the receive cap so an outbound oversize
        // surfaces as a clear local error rather than as a generic peer
        // rejection.
        let cap = frame_size_limits(notebook_doc::frame_types::REQUEST).cap;
        let oversized = vec![0u8; cap + 1];
        let mut buf = Vec::new();
        let err = send_typed_frame(&mut buf, NotebookFrameType::Request, &oversized)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outbound frame too large"));
        // No bytes should have been written for an over-cap frame.
        assert!(
            buf.is_empty(),
            "frame body must not be written when over cap"
        );
    }

    #[test]
    fn frame_size_limits_cover_every_known_frame_type() {
        // Pin the per-type cap table so a new frame type can't slip in
        // without an explicit limit decision. Compares against the
        // outer ceiling so the test fails when an unknown type ends up
        // on the 100 MiB fallback.
        use notebook_doc::frame_types as ft;
        for &ty in &[
            ft::AUTOMERGE_SYNC,
            ft::REQUEST,
            ft::RESPONSE,
            ft::BROADCAST,
            ft::PRESENCE,
            ft::RUNTIME_STATE_SYNC,
            ft::POOL_STATE_SYNC,
            ft::SESSION_CONTROL,
        ] {
            let limits = frame_size_limits(ty);
            assert!(
                limits.cap < MAX_FRAME_SIZE,
                "type 0x{ty:02x} has no tighter cap than the outer ceiling",
            );
            assert!(
                limits.warn < limits.cap,
                "type 0x{ty:02x} warn ({}) >= cap ({})",
                limits.warn,
                limits.cap,
            );
            assert!(limits.warn > 0, "type 0x{ty:02x} warn must be > 0");
        }
    }

    #[tokio::test]
    async fn typed_frame_allows_big_automerge_sync() {
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_rejects_1819243560_byte_length() {
        // The specific desync value observed in the field: 0x6C6F6761
        // ("loga"). Interpreted as a u32 big-endian length this is
        // 1,819,243,560 bytes — well above the 100 MiB outer cap.
        // This must be rejected at the outer check before we ever read
        // the type byte.
        let loga_bytes: [u8; 4] = [0x6C, 0x6F, 0x67, 0x61];
        let mut cursor = std::io::Cursor::new(loga_bytes.to_vec());
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("frame too large"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn test_multiple_frames_on_same_stream() {
        let mut buf = Vec::new();
        send_frame(&mut buf, b"first").await.unwrap();
        send_frame(&mut buf, b"second").await.unwrap();
        send_frame(&mut buf, b"third").await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"first");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"second");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"third");
        // EOF
        assert!(recv_frame(&mut cursor).await.unwrap().is_none());
    }

    #[test]
    fn test_notebook_frame_type_conversion() {
        assert_eq!(
            NotebookFrameType::try_from(0x00).unwrap(),
            NotebookFrameType::AutomergeSync
        );
        assert_eq!(
            NotebookFrameType::try_from(0x01).unwrap(),
            NotebookFrameType::Request
        );
        assert_eq!(
            NotebookFrameType::try_from(0x02).unwrap(),
            NotebookFrameType::Response
        );
        assert_eq!(
            NotebookFrameType::try_from(0x03).unwrap(),
            NotebookFrameType::Broadcast
        );
        assert!(NotebookFrameType::try_from(0xFF).is_err());
    }

    #[tokio::test]
    async fn test_typed_frame_roundtrip() {
        let payload = b"test payload";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Request, payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);
        assert_eq!(frame.payload, payload);
    }

    #[tokio::test]
    async fn test_typed_frame_automerge_sync() {
        let sync_data = b"\x00binary automerge data";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, sync_data)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload, sync_data);
    }

    #[tokio::test]
    async fn test_typed_json_frame() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestMsg {
            value: i32,
        }

        let msg = TestMsg { value: 42 };

        let mut buf = Vec::new();
        send_typed_json_frame(&mut buf, NotebookFrameType::Request, &msg)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);

        let parsed: TestMsg = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn package_manager_as_str_round_trips() {
        assert_eq!(PackageManager::Uv.as_str(), "uv");
        assert_eq!(PackageManager::Conda.as_str(), "conda");
        assert_eq!(PackageManager::Pixi.as_str(), "pixi");
    }

    #[test]
    fn package_manager_parse_valid() {
        assert_eq!(PackageManager::parse("uv").unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::parse("conda").unwrap(),
            PackageManager::Conda
        );
        assert_eq!(PackageManager::parse("pixi").unwrap(), PackageManager::Pixi);
    }

    #[test]
    fn package_manager_parse_aliases() {
        assert_eq!(PackageManager::parse("pip").unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::parse("mamba").unwrap(),
            PackageManager::Conda
        );
    }

    #[test]
    fn package_manager_parse_rejects_unknown() {
        let err = PackageManager::parse("npm").unwrap_err();
        assert!(err.contains("Unsupported package manager 'npm'"));
        assert!(err.contains("Supported: uv, conda, pixi"));
    }

    #[test]
    fn package_manager_fromstr_works() {
        let pm: PackageManager = PackageManager::from_str("conda").unwrap();
        assert_eq!(pm, PackageManager::Conda);
        assert!(PackageManager::from_str("bogus").is_err());
    }

    #[test]
    fn package_manager_display_matches_as_str() {
        assert_eq!(format!("{}", PackageManager::Uv), "uv");
        assert_eq!(format!("{}", PackageManager::Conda), "conda");
        assert_eq!(format!("{}", PackageManager::Pixi), "pixi");
    }

    #[test]
    fn package_manager_serde_is_lowercase() {
        let json = serde_json::to_string(&PackageManager::Conda).unwrap();
        assert_eq!(json, "\"conda\"");
        let pm: PackageManager = serde_json::from_str("\"pixi\"").unwrap();
        assert_eq!(pm, PackageManager::Pixi);
    }

    #[test]
    fn package_manager_deserialize_captures_unknown() {
        // Aliases must decode (wire compatibility for legacy clients).
        let pm: PackageManager = serde_json::from_str("\"pip\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("pip".to_string()));
        let pm: PackageManager = serde_json::from_str("\"mamba\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("mamba".to_string()));
        // Genuinely unknown values decode to Unknown, not an error.
        let pm: PackageManager = serde_json::from_str("\"poetry\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("poetry".to_string()));
    }

    #[test]
    fn package_manager_unknown_round_trips_verbatim() {
        let pm = PackageManager::Unknown("mamba".to_string());
        let json = serde_json::to_string(&pm).unwrap();
        assert_eq!(json, "\"mamba\"");
        let decoded: PackageManager = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, pm);
    }

    #[test]
    fn package_manager_resolve_folds_aliases() {
        assert_eq!(PackageManager::Uv.resolve().unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::Unknown("pip".to_string())
                .resolve()
                .unwrap(),
            PackageManager::Uv
        );
        assert_eq!(
            PackageManager::Unknown("mamba".to_string())
                .resolve()
                .unwrap(),
            PackageManager::Conda
        );
        assert!(PackageManager::Unknown("poetry".to_string())
            .resolve()
            .is_err());
    }

    // -----------------------------------------------------------
    // EnvSource tests
    // -----------------------------------------------------------

    #[test]
    fn env_source_as_str_round_trips_all_variants() {
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Uv).as_str(),
            "uv:prewarmed"
        );
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Conda).as_str(),
            "conda:prewarmed"
        );
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Pixi).as_str(),
            "pixi:prewarmed"
        );
        assert_eq!(EnvSource::Inline(PackageManager::Uv).as_str(), "uv:inline");
        assert_eq!(
            EnvSource::Inline(PackageManager::Conda).as_str(),
            "conda:inline"
        );
        assert_eq!(
            EnvSource::Inline(PackageManager::Pixi).as_str(),
            "pixi:inline"
        );
        assert_eq!(EnvSource::Pyproject.as_str(), "uv:pyproject");
        assert_eq!(EnvSource::PixiToml.as_str(), "pixi:toml");
        assert_eq!(EnvSource::EnvYml.as_str(), "conda:env_yml");
        assert_eq!(EnvSource::Pep723(PackageManager::Uv).as_str(), "uv:pep723");
        assert_eq!(
            EnvSource::Pep723(PackageManager::Pixi).as_str(),
            "pixi:pep723"
        );
        assert_eq!(EnvSource::Deno.as_str(), "deno");
    }

    #[test]
    fn env_source_parse_valid_round_trips() {
        for s in [
            "uv:prewarmed",
            "conda:prewarmed",
            "pixi:prewarmed",
            "uv:inline",
            "conda:inline",
            "pixi:inline",
            "uv:pyproject",
            "pixi:toml",
            "conda:env_yml",
            "uv:pep723",
            "pixi:pep723",
            "deno",
        ] {
            let parsed = EnvSource::parse(s);
            assert_eq!(parsed.as_str(), s, "round-trip failed for {s}");
            assert!(!matches!(parsed, EnvSource::Unknown(_)));
        }
    }

    #[test]
    fn env_source_parse_unknown_captures_string() {
        let pm = EnvSource::parse("weird:future-variant");
        assert_eq!(pm, EnvSource::Unknown("weird:future-variant".to_string()));
        assert_eq!(pm.as_str(), "weird:future-variant");
    }

    #[test]
    fn env_source_parse_empty_is_unknown() {
        assert_eq!(EnvSource::parse(""), EnvSource::Unknown(String::new()));
    }

    #[test]
    fn env_source_serde_is_string() {
        let src = EnvSource::Inline(PackageManager::Conda);
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"conda:inline\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_serde_unknown_round_trips_verbatim() {
        let src = EnvSource::Unknown("something:new".to_string());
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"something:new\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_conda_pep723_round_trips() {
        let src = EnvSource::Pep723(PackageManager::Conda);
        assert_eq!(src.as_str(), "conda:pep723");
        assert_eq!(EnvSource::parse("conda:pep723"), src);
    }

    #[test]
    fn env_source_unknown_prefix_preserves_family() {
        // Forward-compat: newer peers may send env_source strings we haven't
        // taught the enum about. Family classification must still route
        // correctly to the originating package manager's pool / helpers.
        assert_eq!(
            EnvSource::parse("conda:foo").package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::parse("uv:new-source").package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::parse("pixi:bar").package_manager(),
            Some(PackageManager::Pixi)
        );
        // No recognized prefix → no manager.
        assert_eq!(EnvSource::parse("mystery").package_manager(), None);
    }

    #[test]
    fn env_source_package_manager_for_all() {
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Uv).package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::Inline(PackageManager::Conda).package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::Pyproject.package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::PixiToml.package_manager(),
            Some(PackageManager::Pixi)
        );
        assert_eq!(
            EnvSource::EnvYml.package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::Pep723(PackageManager::Pixi).package_manager(),
            Some(PackageManager::Pixi)
        );
        assert_eq!(EnvSource::Deno.package_manager(), None);
        assert_eq!(EnvSource::Unknown("junk".into()).package_manager(), None);
    }

    #[test]
    fn env_source_prepares_own_env() {
        // Inline / project-file / pep723 sources prepare their own env.
        assert!(EnvSource::Inline(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Conda).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Pixi).prepares_own_env());
        assert!(EnvSource::Pyproject.prepares_own_env());
        assert!(EnvSource::PixiToml.prepares_own_env());
        assert!(EnvSource::EnvYml.prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Pixi).prepares_own_env());

        // Prewarmed variants do not prepare their own env.
        assert!(!EnvSource::Prewarmed(PackageManager::Uv).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Conda).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Pixi).prepares_own_env());

        // Deno and Unknown take the "no pool" path.
        assert!(!EnvSource::Deno.prepares_own_env());
        assert!(!EnvSource::Unknown("nope".into()).prepares_own_env());
    }

    // -----------------------------------------------------------
    // LaunchSpec tests
    // -----------------------------------------------------------

    #[test]
    fn launch_spec_parse_auto_variants() {
        assert_eq!(LaunchSpec::parse(""), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("auto"), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("prewarmed"), LaunchSpec::Auto);
        assert_eq!(
            LaunchSpec::parse("auto:uv"),
            LaunchSpec::AutoScoped(PackageManager::Uv)
        );
        assert_eq!(
            LaunchSpec::parse("auto:conda"),
            LaunchSpec::AutoScoped(PackageManager::Conda)
        );
        assert_eq!(
            LaunchSpec::parse("auto:pixi"),
            LaunchSpec::AutoScoped(PackageManager::Pixi)
        );
    }

    #[test]
    fn launch_spec_parse_concrete_delegates_to_env_source() {
        assert_eq!(
            LaunchSpec::parse("uv:inline"),
            LaunchSpec::Concrete(EnvSource::Inline(PackageManager::Uv))
        );
        assert_eq!(
            LaunchSpec::parse("deno"),
            LaunchSpec::Concrete(EnvSource::Deno)
        );
    }

    #[test]
    fn launch_spec_parse_future_value_is_concrete_unknown() {
        assert_eq!(
            LaunchSpec::parse("something:new"),
            LaunchSpec::Concrete(EnvSource::Unknown("something:new".to_string()))
        );
    }

    #[test]
    fn launch_spec_auto_scope_returns_manager() {
        assert_eq!(LaunchSpec::Auto.auto_scope(), None);
        assert_eq!(
            LaunchSpec::AutoScoped(PackageManager::Conda).auto_scope(),
            Some(PackageManager::Conda)
        );
        assert_eq!(LaunchSpec::Concrete(EnvSource::Deno).auto_scope(), None);
    }

    /// `recv_typed_frame` is built on `read_exact`, which is NOT cancel-
    /// safe: dropping the future mid-read silently discards bytes
    /// already pulled off the underlying reader.
    ///
    /// This test exercises the exact misuse pattern that desynced the
    /// runtime-agent ↔ daemon channel under heavy stream output: a peer
    /// loop puts `recv_typed_frame` in a `tokio::select!` arm next to a
    /// high-frequency cancel-safe arm. When the cancel-safe arm wins
    /// while a body read is in flight, the next iteration's
    /// `recv_typed_frame` reads its 4-byte length prefix from the middle
    /// of the previous payload and flags `frame too large` (production
    /// repros saw 0x20202020 from indented kernel stdout and 0x6C6C6F6E
    /// from "...loaded kernel_..." text).
    ///
    /// `FramedReader` is the structural fix: a dedicated reader task
    /// owns the read half and publishes frames through an mpsc, whose
    /// `recv()` is cancel-safe.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn framed_reader_does_not_desync_in_select_under_pressure() {
        use tokio::io::duplex;
        use tokio::sync::mpsc;
        use tokio::time::{timeout, Duration};

        // Tiny duplex buffer forces multi-poll reads, mirroring the real
        // socket pressure that triggered the production desync.
        let (server_side, client_side) = duplex(64);
        let (_server_read, mut writer) = tokio::io::split(server_side);
        let (reader, _client_write) = tokio::io::split(client_side);

        const NUM_FRAMES: usize = 32;
        const PAYLOAD_SIZE: usize = 4096;

        let writer_task = tokio::spawn(async move {
            for i in 0u64..NUM_FRAMES as u64 {
                let mut payload = vec![0u8; PAYLOAD_SIZE];
                payload[..8].copy_from_slice(&i.to_be_bytes());
                for (j, b) in payload[8..].iter_mut().enumerate() {
                    *b = (j & 0xFF) as u8;
                }
                send_typed_frame(&mut writer, NotebookFrameType::AutomergeSync, &payload)
                    .await
                    .expect("writer should not fail");
            }
        });

        // Always-ready cancel-safe interrupter, simulating the real
        // daemon pressure (state_changed_rx fires per kernel output).
        let (interrupter_tx, mut interrupter_rx) = mpsc::unbounded_channel::<()>();
        let pump_token = interrupter_tx.clone();
        let pump_task = tokio::spawn(async move {
            loop {
                if pump_token.send(()).is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let mut framed = FramedReader::spawn(reader, 16);

        let mut received = 0u64;
        let receive_loop = async {
            while received < NUM_FRAMES as u64 {
                tokio::select! {
                    biased;
                    Some(_) = interrupter_rx.recv() => {
                        while interrupter_rx.try_recv().is_ok() {}
                    }
                    maybe = framed.recv() => {
                        let frame = maybe
                            .expect("channel should not close before NUM_FRAMES delivered")
                            .expect("frame decode should not error mid-stream");
                        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
                        assert_eq!(frame.payload.len(), PAYLOAD_SIZE);
                        let seq = u64::from_be_bytes(frame.payload[..8].try_into().unwrap());
                        assert_eq!(
                            seq, received,
                            "frame sequence desynced (expected {}, got {})",
                            received, seq,
                        );
                        for (j, b) in frame.payload[8..].iter().enumerate() {
                            assert_eq!(
                                *b,
                                (j & 0xFF) as u8,
                                "payload corruption at offset {} of frame {}",
                                j + 8,
                                received,
                            );
                        }
                        received += 1;
                    }
                }
            }
        };

        timeout(Duration::from_secs(10), receive_loop)
            .await
            .expect("receive loop should complete within 10s");

        pump_task.abort();
        let _ = pump_task.await;
        writer_task.await.expect("writer task panicked");
        assert_eq!(received, NUM_FRAMES as u64);
    }
}
