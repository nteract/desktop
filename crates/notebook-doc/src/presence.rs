//! Presence: transient peer state for notebooks.
//!
//! Provides binary encode/decode and state tracking for cursor positions,
//! selections, kernel status, and other ephemeral peer information. This
//! module is pure computation with no I/O or timer dependencies — callers
//! supply timestamps as `u64` milliseconds.
//!
//! ## Wire format (frame type 0x04)
//!
//! ```text
//! [1 byte]   message type (0=update, 1=snapshot, 2=left, 3=heartbeat)
//! [2 bytes]  peer_id length (u16 BE)
//! [N bytes]  peer_id (UTF-8)
//! [rest]     payload (type-dependent)
//! ```
//!
//! ### Update payload
//!
//! ```text
//! [1 byte]   channel (0=cursor, 1=selection, 2=kernel_state, 3=custom)
//! [rest]     channel-specific binary data
//! ```
//!
//! ### Snapshot payload
//!
//! ```text
//! [2 bytes]  peer count (u16 BE)
//! [repeat]   for each peer:
//!   [2 bytes]  peer_id length (u16 BE)
//!   [N bytes]  peer_id (UTF-8)
//!   [1 byte]   peer_type (0=wasm, 1=python, 2=mcp, 3=daemon)
//!   [2 bytes]  channel count (u16 BE)
//!   [repeat]   for each channel:
//!     [1 byte]   channel id
//!     [2 bytes]  data length (u16 BE)
//!     [N bytes]  channel data
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ────────────────────────────────────────────────────────

/// Default heartbeat interval in milliseconds (15 seconds).
pub const DEFAULT_HEARTBEAT_MS: u64 = 15_000;

/// Default peer TTL in milliseconds (3× heartbeat = 45 seconds).
pub const DEFAULT_PEER_TTL_MS: u64 = 3 * DEFAULT_HEARTBEAT_MS;

// ── Message types ────────────────────────────────────────────────────

/// Top-level presence message type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Update = 0x00,
    Snapshot = 0x01,
    Left = 0x02,
    Heartbeat = 0x03,
}

impl TryFrom<u8> for MessageType {
    type Error = PresenceError;
    fn try_from(b: u8) -> Result<Self, Self::Error> {
        match b {
            0x00 => Ok(Self::Update),
            0x01 => Ok(Self::Snapshot),
            0x02 => Ok(Self::Left),
            0x03 => Ok(Self::Heartbeat),
            _ => Err(PresenceError::InvalidMessageType(b)),
        }
    }
}

/// Channel identifier byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Channel {
    Cursor = 0x00,
    Selection = 0x01,
    KernelState = 0x02,
    Custom = 0x03,
}

impl TryFrom<u8> for Channel {
    type Error = PresenceError;
    fn try_from(b: u8) -> Result<Self, Self::Error> {
        match b {
            0x00 => Ok(Self::Cursor),
            0x01 => Ok(Self::Selection),
            0x02 => Ok(Self::KernelState),
            0x03 => Ok(Self::Custom),
            _ => Err(PresenceError::InvalidChannel(b)),
        }
    }
}

/// Peer type byte (for snapshot metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PeerType {
    Wasm = 0x00,
    Python = 0x01,
    Mcp = 0x02,
    Daemon = 0x03,
}

impl TryFrom<u8> for PeerType {
    type Error = PresenceError;
    fn try_from(b: u8) -> Result<Self, Self::Error> {
        match b {
            0x00 => Ok(Self::Wasm),
            0x01 => Ok(Self::Python),
            0x02 => Ok(Self::Mcp),
            0x03 => Ok(Self::Daemon),
            _ => Err(PresenceError::InvalidPeerType(b)),
        }
    }
}

/// Kernel status byte (for kernel state channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum KernelStatus {
    NotStarted = 0x00,
    Starting = 0x01,
    Idle = 0x02,
    Busy = 0x03,
    Errored = 0x04,
    Shutdown = 0x05,
}

impl TryFrom<u8> for KernelStatus {
    type Error = PresenceError;
    fn try_from(b: u8) -> Result<Self, <Self as TryFrom<u8>>::Error> {
        match b {
            0x00 => Ok(Self::NotStarted),
            0x01 => Ok(Self::Starting),
            0x02 => Ok(Self::Idle),
            0x03 => Ok(Self::Busy),
            0x04 => Ok(Self::Errored),
            0x05 => Ok(Self::Shutdown),
            _ => Err(PresenceError::InvalidKernelStatus(b)),
        }
    }
}

impl KernelStatus {
    /// Parse from a status string (as used in protocol broadcasts).
    pub fn from_str(s: &str) -> Self {
        match s {
            "starting" => Self::Starting,
            "idle" => Self::Idle,
            "busy" => Self::Busy,
            "error" => Self::Errored,
            "shutdown" => Self::Shutdown,
            _ => Self::NotStarted,
        }
    }

    /// Convert to the status string used in protocol broadcasts.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Starting => "starting",
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Errored => "error",
            Self::Shutdown => "shutdown",
        }
    }
}

// ── Channel data types ───────────────────────────────────────────────

/// Cursor position within a cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub cell_id: String,
    pub line: u32,
    pub column: u32,
}

/// Selection range within a cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRange {
    pub cell_id: String,
    pub anchor_line: u32,
    pub anchor_col: u32,
    pub head_line: u32,
    pub head_col: u32,
}

/// Kernel state (daemon-owned presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelStateData {
    pub status: KernelStatus,
    pub env_source: String,
}

/// Decoded channel data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelData {
    Cursor(CursorPosition),
    Selection(SelectionRange),
    KernelState(KernelStateData),
    Custom(Vec<u8>),
}

// ── Decoded messages ─────────────────────────────────────────────────

/// A fully decoded presence message.
#[derive(Debug, Clone)]
pub enum PresenceMessage {
    Update {
        peer_id: String,
        channel: Channel,
        data: ChannelData,
    },
    Snapshot {
        peer_id: String,
        peers: Vec<PeerSnapshot>,
    },
    Left {
        peer_id: String,
    },
    Heartbeat {
        peer_id: String,
    },
}

/// A peer's full state within a snapshot.
#[derive(Debug, Clone)]
pub struct PeerSnapshot {
    pub peer_id: String,
    pub peer_type: PeerType,
    pub channels: Vec<(Channel, Vec<u8>)>,
}

// ── Errors ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    #[error("buffer too short: need {need} bytes, have {have}")]
    BufferTooShort { need: usize, have: usize },
    #[error("invalid message type: 0x{0:02X}")]
    InvalidMessageType(u8),
    #[error("invalid channel: 0x{0:02X}")]
    InvalidChannel(u8),
    #[error("invalid peer type: 0x{0:02X}")]
    InvalidPeerType(u8),
    #[error("invalid kernel status: 0x{0:02X}")]
    InvalidKernelStatus(u8),
    #[error("invalid UTF-8 in peer_id")]
    InvalidPeerId,
    #[error("invalid UTF-8 in string field")]
    InvalidUtf8,
}

// ── Binary encoding ──────────────────────────────────────────────────

/// Encode a cursor update message.
pub fn encode_cursor_update(peer_id: &str, pos: &CursorPosition) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len() + 1 + 2 + pos.cell_id.len() + 8);
    buf.push(MessageType::Update as u8);
    write_str(&mut buf, peer_id);
    buf.push(Channel::Cursor as u8);
    write_str(&mut buf, &pos.cell_id);
    buf.extend_from_slice(&pos.line.to_be_bytes());
    buf.extend_from_slice(&pos.column.to_be_bytes());
    buf
}

/// Encode a selection update message.
pub fn encode_selection_update(peer_id: &str, sel: &SelectionRange) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len() + 1 + 2 + sel.cell_id.len() + 16);
    buf.push(MessageType::Update as u8);
    write_str(&mut buf, peer_id);
    buf.push(Channel::Selection as u8);
    write_str(&mut buf, &sel.cell_id);
    buf.extend_from_slice(&sel.anchor_line.to_be_bytes());
    buf.extend_from_slice(&sel.anchor_col.to_be_bytes());
    buf.extend_from_slice(&sel.head_line.to_be_bytes());
    buf.extend_from_slice(&sel.head_col.to_be_bytes());
    buf
}

/// Encode a kernel state update message (daemon-owned).
pub fn encode_kernel_state_update(peer_id: &str, state: &KernelStateData) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len() + 1 + 1 + 2 + state.env_source.len());
    buf.push(MessageType::Update as u8);
    write_str(&mut buf, peer_id);
    buf.push(Channel::KernelState as u8);
    buf.push(state.status as u8);
    write_str(&mut buf, &state.env_source);
    buf
}

/// Encode a custom channel update message (JSON fallback).
pub fn encode_custom_update(peer_id: &str, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len() + 1 + data.len());
    buf.push(MessageType::Update as u8);
    write_str(&mut buf, peer_id);
    buf.push(Channel::Custom as u8);
    buf.extend_from_slice(data);
    buf
}

/// Encode a heartbeat message.
pub fn encode_heartbeat(peer_id: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len());
    buf.push(MessageType::Heartbeat as u8);
    write_str(&mut buf, peer_id);
    buf
}

/// Encode a "peer left" message.
pub fn encode_left(peer_id: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 2 + peer_id.len());
    buf.push(MessageType::Left as u8);
    write_str(&mut buf, peer_id);
    buf
}

/// Encode a full snapshot of all peers' presence state.
pub fn encode_snapshot(sender_peer_id: &str, peers: &[PeerSnapshot]) -> Vec<u8> {
    // Pre-calculate size estimate
    let mut size = 1 + 2 + sender_peer_id.len() + 2;
    for peer in peers {
        size += 2 + peer.peer_id.len() + 1 + 2;
        for (_, data) in &peer.channels {
            size += 1 + 2 + data.len();
        }
    }

    let mut buf = Vec::with_capacity(size);
    buf.push(MessageType::Snapshot as u8);
    write_str(&mut buf, sender_peer_id);
    write_u16(&mut buf, peers.len() as u16);

    for peer in peers {
        write_str(&mut buf, &peer.peer_id);
        buf.push(peer.peer_type as u8);
        write_u16(&mut buf, peer.channels.len() as u16);
        for (channel, data) in &peer.channels {
            buf.push(*channel as u8);
            write_u16(&mut buf, data.len() as u16);
            buf.extend_from_slice(data);
        }
    }

    buf
}

// ── Binary decoding ──────────────────────────────────────────────────

/// Decode a presence message from bytes.
pub fn decode_message(data: &[u8]) -> Result<PresenceMessage, PresenceError> {
    let mut pos = 0;
    let msg_type = read_u8(data, &mut pos)?;
    let msg_type = MessageType::try_from(msg_type)?;
    let peer_id = read_str(data, &mut pos)?;

    match msg_type {
        MessageType::Update => {
            let channel_byte = read_u8(data, &mut pos)?;
            let channel = Channel::try_from(channel_byte)?;
            let channel_data = decode_channel_data(channel, data, &mut pos)?;
            Ok(PresenceMessage::Update {
                peer_id,
                channel,
                data: channel_data,
            })
        }
        MessageType::Snapshot => {
            let peer_count = read_u16(data, &mut pos)?;
            let mut peers = Vec::with_capacity(peer_count as usize);
            for _ in 0..peer_count {
                let snap_peer_id = read_str(data, &mut pos)?;
                let peer_type_byte = read_u8(data, &mut pos)?;
                let peer_type = PeerType::try_from(peer_type_byte)?;
                let channel_count = read_u16(data, &mut pos)?;
                let mut channels = Vec::with_capacity(channel_count as usize);
                for _ in 0..channel_count {
                    let ch_byte = read_u8(data, &mut pos)?;
                    let ch = Channel::try_from(ch_byte)?;
                    let data_len = read_u16(data, &mut pos)? as usize;
                    let ch_data = read_bytes(data, &mut pos, data_len)?;
                    channels.push((ch, ch_data.to_vec()));
                }
                peers.push(PeerSnapshot {
                    peer_id: snap_peer_id,
                    peer_type,
                    channels,
                });
            }
            Ok(PresenceMessage::Snapshot { peer_id, peers })
        }
        MessageType::Left => Ok(PresenceMessage::Left { peer_id }),
        MessageType::Heartbeat => Ok(PresenceMessage::Heartbeat { peer_id }),
    }
}

/// Decode channel-specific data from the update payload.
pub fn decode_channel_data(
    channel: Channel,
    data: &[u8],
    pos: &mut usize,
) -> Result<ChannelData, PresenceError> {
    match channel {
        Channel::Cursor => {
            let cell_id = read_str(data, pos)?;
            let line = read_u32(data, pos)?;
            let column = read_u32(data, pos)?;
            Ok(ChannelData::Cursor(CursorPosition {
                cell_id,
                line,
                column,
            }))
        }
        Channel::Selection => {
            let cell_id = read_str(data, pos)?;
            let anchor_line = read_u32(data, pos)?;
            let anchor_col = read_u32(data, pos)?;
            let head_line = read_u32(data, pos)?;
            let head_col = read_u32(data, pos)?;
            Ok(ChannelData::Selection(SelectionRange {
                cell_id,
                anchor_line,
                anchor_col,
                head_line,
                head_col,
            }))
        }
        Channel::KernelState => {
            let status_byte = read_u8(data, pos)?;
            let status = KernelStatus::try_from(status_byte)?;
            let env_source = read_str(data, pos)?;
            Ok(ChannelData::KernelState(KernelStateData {
                status,
                env_source,
            }))
        }
        Channel::Custom => {
            let remaining = &data[*pos..];
            *pos = data.len();
            Ok(ChannelData::Custom(remaining.to_vec()))
        }
    }
}

/// Encode a single channel's data (for use in snapshots).
pub fn encode_channel_data(data: &ChannelData) -> (Channel, Vec<u8>) {
    match data {
        ChannelData::Cursor(c) => {
            let mut buf = Vec::with_capacity(2 + c.cell_id.len() + 8);
            write_str(&mut buf, &c.cell_id);
            buf.extend_from_slice(&c.line.to_be_bytes());
            buf.extend_from_slice(&c.column.to_be_bytes());
            (Channel::Cursor, buf)
        }
        ChannelData::Selection(s) => {
            let mut buf = Vec::with_capacity(2 + s.cell_id.len() + 16);
            write_str(&mut buf, &s.cell_id);
            buf.extend_from_slice(&s.anchor_line.to_be_bytes());
            buf.extend_from_slice(&s.anchor_col.to_be_bytes());
            buf.extend_from_slice(&s.head_line.to_be_bytes());
            buf.extend_from_slice(&s.head_col.to_be_bytes());
            (Channel::Selection, buf)
        }
        ChannelData::KernelState(k) => {
            let mut buf = Vec::with_capacity(1 + 2 + k.env_source.len());
            buf.push(k.status as u8);
            write_str(&mut buf, &k.env_source);
            (Channel::KernelState, buf)
        }
        ChannelData::Custom(bytes) => (Channel::Custom, bytes.clone()),
    }
}

// ── Presence State ───────────────────────────────────────────────────

/// Per-peer presence info tracked by the state manager.
#[derive(Debug, Clone)]
pub struct PeerPresence {
    pub peer_id: String,
    pub peer_type: PeerType,
    pub channels: HashMap<Channel, ChannelData>,
    /// Last time this peer sent any message (update or heartbeat), in ms.
    pub last_seen_ms: u64,
    /// Last time this peer sent a meaningful update (not heartbeat), in ms.
    pub last_active_ms: u64,
}

/// Manages presence state for all peers in a notebook room.
///
/// This is a pure data structure with no I/O. Callers provide timestamps
/// as `u64` milliseconds and are responsible for scheduling heartbeats
/// and prune checks.
#[derive(Debug, Clone, Default)]
pub struct PresenceState {
    peers: HashMap<String, PeerPresence>,
    /// Timestamp of the last heartbeat we sent, in ms.
    last_heartbeat_ms: u64,
}

impl PresenceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update a peer's channel data. Creates the peer if not seen before.
    ///
    /// Returns `true` if this is a new peer (caller may want to send a snapshot).
    pub fn update_peer(
        &mut self,
        peer_id: &str,
        peer_type: PeerType,
        channel: Channel,
        data: ChannelData,
        now_ms: u64,
    ) -> bool {
        let is_new = !self.peers.contains_key(peer_id);
        let entry = self
            .peers
            .entry(peer_id.to_string())
            .or_insert_with(|| PeerPresence {
                peer_id: peer_id.to_string(),
                peer_type,
                channels: HashMap::new(),
                last_seen_ms: now_ms,
                last_active_ms: now_ms,
            });
        entry.channels.insert(channel, data);
        entry.last_seen_ms = now_ms;
        entry.last_active_ms = now_ms;
        entry.peer_type = peer_type;
        is_new
    }

    /// Mark a peer as seen (heartbeat received) without updating channel data.
    pub fn mark_seen(&mut self, peer_id: &str, now_ms: u64) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.last_seen_ms = now_ms;
        }
        // Ignore heartbeats from unknown peers (they should send an update first).
    }

    /// Remove a peer (explicit disconnect).
    pub fn remove_peer(&mut self, peer_id: &str) -> Option<PeerPresence> {
        self.peers.remove(peer_id)
    }

    /// Prune peers that haven't been seen within the TTL.
    ///
    /// Returns the peer IDs that were removed.
    pub fn prune_stale(&mut self, now_ms: u64, ttl_ms: u64) -> Vec<String> {
        let mut pruned = Vec::new();
        self.peers.retain(|peer_id, peer| {
            if now_ms.saturating_sub(peer.last_seen_ms) > ttl_ms {
                pruned.push(peer_id.clone());
                false
            } else {
                true
            }
        });
        pruned
    }

    /// Check if it's time to send a heartbeat.
    pub fn should_heartbeat(&self, now_ms: u64, interval_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_heartbeat_ms) >= interval_ms
    }

    /// Record that we sent a heartbeat.
    pub fn record_heartbeat(&mut self, now_ms: u64) {
        self.last_heartbeat_ms = now_ms;
    }

    /// Get all peers.
    pub fn peers(&self) -> &HashMap<String, PeerPresence> {
        &self.peers
    }

    /// Get a specific peer's presence.
    pub fn get_peer(&self, peer_id: &str) -> Option<&PeerPresence> {
        self.peers.get(peer_id)
    }

    /// Get the number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Build a snapshot of all peers' current state, encoded as bytes.
    pub fn encode_snapshot(&self, sender_peer_id: &str) -> Vec<u8> {
        let snapshots: Vec<PeerSnapshot> = self
            .peers
            .values()
            .map(|peer| {
                let channels: Vec<(Channel, Vec<u8>)> = peer
                    .channels
                    .iter()
                    .map(|(ch, data)| {
                        let (_, encoded) = encode_channel_data(data);
                        (*ch, encoded)
                    })
                    .collect();
                PeerSnapshot {
                    peer_id: peer.peer_id.clone(),
                    peer_type: peer.peer_type,
                    channels,
                }
            })
            .collect();
        encode_snapshot(sender_peer_id, &snapshots)
    }

    /// Apply a decoded snapshot, replacing state for all included peers.
    pub fn apply_snapshot(&mut self, peers: &[PeerSnapshot], now_ms: u64) {
        for snap in peers {
            let mut channels = HashMap::new();
            for (ch, data) in &snap.channels {
                let mut pos = 0;
                if let Ok(decoded) = decode_channel_data(*ch, data, &mut pos) {
                    channels.insert(*ch, decoded);
                }
            }
            self.peers.insert(
                snap.peer_id.clone(),
                PeerPresence {
                    peer_id: snap.peer_id.clone(),
                    peer_type: snap.peer_type,
                    channels,
                    last_seen_ms: now_ms,
                    last_active_ms: now_ms,
                },
            );
        }
    }

    /// Get the kernel state from the daemon peer, if present.
    pub fn kernel_state(&self) -> Option<&KernelStateData> {
        for peer in self.peers.values() {
            if peer.peer_type == PeerType::Daemon {
                if let Some(ChannelData::KernelState(ks)) = peer.channels.get(&Channel::KernelState)
                {
                    return Some(ks);
                }
            }
        }
        None
    }

    /// Get all cursor positions (excluding a specific peer).
    pub fn remote_cursors(&self, exclude_peer: &str) -> Vec<(&str, &CursorPosition)> {
        self.peers
            .values()
            .filter(|p| p.peer_id != exclude_peer)
            .filter_map(|p| {
                if let Some(ChannelData::Cursor(ref c)) = p.channels.get(&Channel::Cursor) {
                    Some((p.peer_id.as_str(), c))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all selection ranges (excluding a specific peer).
    pub fn remote_selections(&self, exclude_peer: &str) -> Vec<(&str, &SelectionRange)> {
        self.peers
            .values()
            .filter(|p| p.peer_id != exclude_peer)
            .filter_map(|p| {
                if let Some(ChannelData::Selection(ref s)) = p.channels.get(&Channel::Selection) {
                    Some((p.peer_id.as_str(), s))
                } else {
                    None
                }
            })
            .collect()
    }
}

// ── Binary helpers ───────────────────────────────────────────────────

fn write_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u16(buf, s.len() as u16);
    buf.extend_from_slice(s.as_bytes());
}

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, PresenceError> {
    if *pos >= data.len() {
        return Err(PresenceError::BufferTooShort {
            need: *pos + 1,
            have: data.len(),
        });
    }
    let val = data[*pos];
    *pos += 1;
    Ok(val)
}

fn read_u16(data: &[u8], pos: &mut usize) -> Result<u16, PresenceError> {
    if *pos + 2 > data.len() {
        return Err(PresenceError::BufferTooShort {
            need: *pos + 2,
            have: data.len(),
        });
    }
    let val = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Ok(val)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, PresenceError> {
    if *pos + 4 > data.len() {
        return Err(PresenceError::BufferTooShort {
            need: *pos + 4,
            have: data.len(),
        });
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], PresenceError> {
    if *pos + len > data.len() {
        return Err(PresenceError::BufferTooShort {
            need: *pos + len,
            have: data.len(),
        });
    }
    let slice = &data[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

fn read_str(data: &[u8], pos: &mut usize) -> Result<String, PresenceError> {
    let len = read_u16(data, pos)? as usize;
    let bytes = read_bytes(data, pos, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| PresenceError::InvalidUtf8)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_roundtrip() {
        let pos = CursorPosition {
            cell_id: "cell-abc".into(),
            line: 42,
            column: 7,
        };
        let encoded = encode_cursor_update("peer-1", &pos);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Update {
                peer_id,
                channel,
                data,
            } => {
                assert_eq!(peer_id, "peer-1");
                assert_eq!(channel, Channel::Cursor);
                assert_eq!(data, ChannelData::Cursor(pos));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_selection_roundtrip() {
        let sel = SelectionRange {
            cell_id: "cell-xyz".into(),
            anchor_line: 1,
            anchor_col: 0,
            head_line: 5,
            head_col: 20,
        };
        let encoded = encode_selection_update("editor-2", &sel);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Update {
                peer_id,
                channel,
                data,
            } => {
                assert_eq!(peer_id, "editor-2");
                assert_eq!(channel, Channel::Selection);
                assert_eq!(data, ChannelData::Selection(sel));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_kernel_state_roundtrip() {
        let ks = KernelStateData {
            status: KernelStatus::Idle,
            env_source: "uv:prewarmed".into(),
        };
        let encoded = encode_kernel_state_update("daemon", &ks);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Update {
                peer_id,
                channel,
                data,
            } => {
                assert_eq!(peer_id, "daemon");
                assert_eq!(channel, Channel::KernelState);
                assert_eq!(data, ChannelData::KernelState(ks));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let encoded = encode_heartbeat("peer-3");
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Heartbeat { peer_id } => assert_eq!(peer_id, "peer-3"),
            _ => panic!("expected Heartbeat"),
        }
    }

    #[test]
    fn test_left_roundtrip() {
        let encoded = encode_left("peer-gone");
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Left { peer_id } => assert_eq!(peer_id, "peer-gone"),
            _ => panic!("expected Left"),
        }
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let cursor_data = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 10,
            column: 5,
        });
        let kernel_data = ChannelData::KernelState(KernelStateData {
            status: KernelStatus::Busy,
            env_source: "conda:prewarmed".into(),
        });

        let (cursor_ch, cursor_bytes) = encode_channel_data(&cursor_data);
        let (kernel_ch, kernel_bytes) = encode_channel_data(&kernel_data);

        let peers = vec![
            PeerSnapshot {
                peer_id: "user-1".into(),
                peer_type: PeerType::Wasm,
                channels: vec![(cursor_ch, cursor_bytes)],
            },
            PeerSnapshot {
                peer_id: "daemon".into(),
                peer_type: PeerType::Daemon,
                channels: vec![(kernel_ch, kernel_bytes)],
            },
        ];

        let encoded = encode_snapshot("daemon", &peers);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Snapshot { peer_id, peers } => {
                assert_eq!(peer_id, "daemon");
                assert_eq!(peers.len(), 2);
                assert_eq!(peers[0].peer_id, "user-1");
                assert_eq!(peers[0].peer_type, PeerType::Wasm);
                assert_eq!(peers[0].channels.len(), 1);
                assert_eq!(peers[1].peer_id, "daemon");
                assert_eq!(peers[1].peer_type, PeerType::Daemon);
            }
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn test_custom_channel_roundtrip() {
        let custom = b"{\"foo\":\"bar\"}";
        let encoded = encode_custom_update("agent-x", custom);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Update {
                peer_id,
                channel,
                data,
            } => {
                assert_eq!(peer_id, "agent-x");
                assert_eq!(channel, Channel::Custom);
                assert_eq!(data, ChannelData::Custom(custom.to_vec()));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_cursor_update_is_compact() {
        let pos = CursorPosition {
            cell_id: "c1".into(),
            line: 10,
            column: 5,
        };
        let encoded = encode_cursor_update("p1", &pos);
        // 1 (type) + 2+2 (peer_id) + 1 (channel) + 2+2 (cell_id) + 4+4 (line+col) = 18
        assert_eq!(encoded.len(), 18);
    }

    // ── PresenceState tests ──────────────────────────────────────────

    #[test]
    fn test_state_update_and_get() {
        let mut state = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 1,
            column: 0,
        });
        let is_new = state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, cursor, 1000);
        assert!(is_new);
        assert_eq!(state.peer_count(), 1);
        assert!(state.get_peer("peer-1").is_some());
    }

    #[test]
    fn test_state_update_existing_peer() {
        let mut state = PresenceState::new();
        let c1 = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 1,
            column: 0,
        });
        let c2 = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 5,
            column: 10,
        });
        assert!(state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, c1, 1000));
        assert!(!state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, c2, 2000));
        assert_eq!(state.peer_count(), 1);
        let peer = state.get_peer("peer-1").unwrap();
        assert_eq!(peer.last_active_ms, 2000);
        match &peer.channels[&Channel::Cursor] {
            ChannelData::Cursor(c) => assert_eq!(c.line, 5),
            _ => panic!("expected cursor"),
        }
    }

    #[test]
    fn test_state_prune_stale() {
        let mut state = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 0,
            column: 0,
        });
        state.update_peer("old", PeerType::Wasm, Channel::Cursor, cursor.clone(), 1000);
        state.update_peer("recent", PeerType::Python, Channel::Cursor, cursor, 50_000);

        let pruned = state.prune_stale(60_000, DEFAULT_PEER_TTL_MS);
        assert_eq!(pruned, vec!["old"]);
        assert_eq!(state.peer_count(), 1);
        assert!(state.get_peer("recent").is_some());
    }

    #[test]
    fn test_state_heartbeat_keeps_alive() {
        let mut state = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 0,
            column: 0,
        });
        state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, cursor, 1000);

        // Heartbeat at 30s keeps it alive
        state.mark_seen("peer-1", 30_000);

        // Prune at 60s — peer was seen at 30s, TTL is 45s, so 60-30=30 < 45
        let pruned = state.prune_stale(60_000, DEFAULT_PEER_TTL_MS);
        assert!(pruned.is_empty());
    }

    #[test]
    fn test_state_remove_peer() {
        let mut state = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 0,
            column: 0,
        });
        state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, cursor, 1000);
        assert_eq!(state.peer_count(), 1);
        let removed = state.remove_peer("peer-1");
        assert!(removed.is_some());
        assert_eq!(state.peer_count(), 0);
    }

    #[test]
    fn test_state_kernel_state_accessor() {
        let mut state = PresenceState::new();
        assert!(state.kernel_state().is_none());

        let ks = ChannelData::KernelState(KernelStateData {
            status: KernelStatus::Idle,
            env_source: "uv:inline".into(),
        });
        state.update_peer("daemon", PeerType::Daemon, Channel::KernelState, ks, 1000);

        let kernel = state.kernel_state().unwrap();
        assert_eq!(kernel.status, KernelStatus::Idle);
        assert_eq!(kernel.env_source, "uv:inline");
    }

    #[test]
    fn test_state_remote_cursors() {
        let mut state = PresenceState::new();
        let c1 = ChannelData::Cursor(CursorPosition {
            cell_id: "cell-a".into(),
            line: 1,
            column: 0,
        });
        let c2 = ChannelData::Cursor(CursorPosition {
            cell_id: "cell-b".into(),
            line: 5,
            column: 3,
        });
        state.update_peer("me", PeerType::Wasm, Channel::Cursor, c1, 1000);
        state.update_peer("agent", PeerType::Mcp, Channel::Cursor, c2, 1000);

        let cursors = state.remote_cursors("me");
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].0, "agent");
        assert_eq!(cursors[0].1.cell_id, "cell-b");
    }

    #[test]
    fn test_state_should_heartbeat() {
        let mut state = PresenceState::new();
        // last_heartbeat_ms starts at 0, so at t=0 the interval hasn't elapsed
        assert!(!state.should_heartbeat(0, DEFAULT_HEARTBEAT_MS));
        // At t=15000 the interval has elapsed
        assert!(state.should_heartbeat(DEFAULT_HEARTBEAT_MS, DEFAULT_HEARTBEAT_MS));
        state.record_heartbeat(DEFAULT_HEARTBEAT_MS);
        // 5s after the heartbeat — not yet
        assert!(!state.should_heartbeat(DEFAULT_HEARTBEAT_MS + 5000, DEFAULT_HEARTBEAT_MS));
        // 15s after the heartbeat — time again
        assert!(state.should_heartbeat(
            DEFAULT_HEARTBEAT_MS + DEFAULT_HEARTBEAT_MS,
            DEFAULT_HEARTBEAT_MS
        ));
    }

    #[test]
    fn test_state_encode_and_apply_snapshot() {
        let mut state1 = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 10,
            column: 5,
        });
        let ks = ChannelData::KernelState(KernelStateData {
            status: KernelStatus::Busy,
            env_source: "conda:prewarmed".into(),
        });
        state1.update_peer("user", PeerType::Wasm, Channel::Cursor, cursor, 1000);
        state1.update_peer("daemon", PeerType::Daemon, Channel::KernelState, ks, 1000);

        let snapshot_bytes = state1.encode_snapshot("daemon");

        // Decode and apply to a fresh state
        let msg = decode_message(&snapshot_bytes).unwrap();
        let mut state2 = PresenceState::new();
        if let PresenceMessage::Snapshot { peers, .. } = msg {
            state2.apply_snapshot(&peers, 2000);
        } else {
            panic!("expected snapshot");
        }

        assert_eq!(state2.peer_count(), 2);
        assert!(state2.get_peer("user").is_some());
        assert!(state2.get_peer("daemon").is_some());
        let kernel = state2.kernel_state().unwrap();
        assert_eq!(kernel.status, KernelStatus::Busy);

        let cursors = state2.remote_cursors("nobody");
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].1.line, 10);
    }

    #[test]
    fn test_empty_buffer_error() {
        let result = decode_message(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_truncated_cursor_error() {
        // Valid header but truncated cursor payload
        let mut buf = vec![MessageType::Update as u8];
        write_str(&mut buf, "p1");
        buf.push(Channel::Cursor as u8);
        write_str(&mut buf, "cell");
        // Missing line + column bytes
        let result = decode_message(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_channels_per_peer() {
        let mut state = PresenceState::new();
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 1,
            column: 0,
        });
        let sel = ChannelData::Selection(SelectionRange {
            cell_id: "c1".into(),
            anchor_line: 1,
            anchor_col: 0,
            head_line: 3,
            head_col: 10,
        });
        state.update_peer("peer-1", PeerType::Wasm, Channel::Cursor, cursor, 1000);
        state.update_peer("peer-1", PeerType::Wasm, Channel::Selection, sel, 1000);

        let peer = state.get_peer("peer-1").unwrap();
        assert_eq!(peer.channels.len(), 2);
        assert!(peer.channels.contains_key(&Channel::Cursor));
        assert!(peer.channels.contains_key(&Channel::Selection));
    }
}
