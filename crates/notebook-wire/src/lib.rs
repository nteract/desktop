//! Low-level wire constants and control-frame types for notebook connections.
//!
//! This crate deliberately contains no document schema or daemon/client I/O.
//! It is the common layer for crates that need to agree on frame bytes,
//! frame-size caps, and connection-local session status shapes.

use serde::{Deserialize, Serialize};

pub mod frame_types {
    //! First-byte typed-frame constants for notebook sync connections.

    /// Automerge sync message (binary).
    pub const AUTOMERGE_SYNC: u8 = 0x00;

    /// NotebookRequest (JSON).
    pub const REQUEST: u8 = 0x01;

    /// NotebookResponse (JSON).
    pub const RESPONSE: u8 = 0x02;

    /// NotebookBroadcast (JSON).
    pub const BROADCAST: u8 = 0x03;

    /// Presence (CBOR).
    pub const PRESENCE: u8 = 0x04;

    /// RuntimeStateDoc sync message (binary Automerge sync).
    pub const RUNTIME_STATE_SYNC: u8 = 0x05;

    /// PoolDoc sync message (binary Automerge sync).
    pub const POOL_STATE_SYNC: u8 = 0x06;

    /// Session-control message (JSON).
    pub const SESSION_CONTROL: u8 = 0x07;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// Outer ceiling for any frame.
pub const MAX_FRAME_SIZE: usize = 100 * MIB;

/// Maximum frame size for control/handshake frames.
pub const MAX_CONTROL_FRAME_SIZE: usize = 64 * KIB;

/// Per-type body cap and warn threshold for a typed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSizeLimits {
    /// Hard cap. Frames exceeding this are rejected.
    pub cap: usize,
    /// Soft threshold. Frames exceeding this log a warning but proceed.
    pub warn: usize,
}

/// Per-type body limits for typed frames.
pub fn frame_size_limits(type_byte: u8) -> FrameSizeLimits {
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
        _ => FrameSizeLimits {
            cap: MAX_FRAME_SIZE,
            warn: MAX_FRAME_SIZE / 2,
        },
    }
}

/// Minimum protocol version accepted by v4 daemons.
pub const MIN_PROTOCOL_VERSION: u32 = 4;

/// Magic bytes identifying the runtimed protocol.
pub const MAGIC: [u8; 4] = [0xC0, 0xDE, 0x01, 0xAC];

/// Total preamble size: 4-byte magic + 1-byte protocol version.
pub const PREAMBLE_LEN: usize = 5;

/// Frame types for notebook sync connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotebookFrameType {
    /// Automerge sync message (binary).
    AutomergeSync = frame_types::AUTOMERGE_SYNC,
    /// NotebookRequest (JSON).
    Request = frame_types::REQUEST,
    /// NotebookResponse (JSON).
    Response = frame_types::RESPONSE,
    /// NotebookBroadcast (JSON).
    Broadcast = frame_types::BROADCAST,
    /// Presence (CBOR).
    Presence = frame_types::PRESENCE,
    /// RuntimeStateDoc sync message (binary Automerge sync).
    RuntimeStateSync = frame_types::RUNTIME_STATE_SYNC,
    /// PoolDoc sync message (binary Automerge sync, global).
    PoolStateSync = frame_types::POOL_STATE_SYNC,
    /// Session-control message (JSON, server-originated connection status).
    SessionControl = frame_types::SESSION_CONTROL,
}

impl TryFrom<u8> for NotebookFrameType {
    type Error = std::io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
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

/// Session-control messages sent by the daemon on the notebook sync socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionControlMessage {
    SyncStatus(SessionSyncStatusWire),
}

/// Full connection bootstrap/readiness snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSyncStatusWire {
    pub notebook_doc: NotebookDocPhaseWire,
    pub runtime_state: RuntimeStatePhaseWire,
    pub initial_load: InitialLoadPhaseWire,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NotebookDocPhaseWire {
    Pending,
    Syncing,
    Interactive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatePhaseWire {
    Pending,
    Syncing,
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum InitialLoadPhaseWire {
    NotNeeded,
    Streaming,
    Ready,
    Failed { reason: String },
}
