//! Presence: transient peer state for notebooks.
//!
//! Provides CBOR encode/decode and state tracking for cursor positions,
//! selections, kernel status, and other ephemeral peer information. This
//! module is pure computation with no I/O or timer dependencies — callers
//! supply timestamps as `u64` milliseconds.
//!
//! ## Wire format (frame type 0x04)
//!
//! All presence messages are CBOR-encoded, matching the approach used by
//! automerge-repo for ephemeral messages. This gives us a self-describing
//! binary format with schema evolution — new fields are silently skipped
//! by old decoders.
//!
//! Messages are tagged with a `type` field:
//!
//! - `"update"` — single channel value from one peer (cursor, selection, kernel state, custom)
//! - `"snapshot"` — full state for all peers, sent to late joiners
//! - `"left"` — peer disconnected
//! - `"heartbeat"` — keep-alive for TTL-based pruning

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ────────────────────────────────────────────────────────

/// Default heartbeat interval in milliseconds (15 seconds).
pub const DEFAULT_HEARTBEAT_MS: u64 = 15_000;

/// Default peer TTL in milliseconds (3× heartbeat = 45 seconds).
pub const DEFAULT_PEER_TTL_MS: u64 = 3 * DEFAULT_HEARTBEAT_MS;

/// Maximum size for a presence frame payload (4 KiB).
/// Presence data is small (cursor ~40 bytes CBOR, selection ~60 bytes).
/// Anything larger is likely malformed or malicious.
pub const MAX_PRESENCE_FRAME_SIZE: usize = 4 * 1024;

// ── Channel types ────────────────────────────────────────────────────

/// Channel identifier for presence updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Cursor,
    Selection,
    KernelState,
    Custom,
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

/// Kernel status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelStatus {
    NotStarted,
    Starting,
    Idle,
    Busy,
    Errored,
    Shutdown,
}

impl KernelStatus {
    /// Parse from a status string (as used in protocol broadcasts).
    pub fn from_status_str(s: &str) -> Self {
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

/// Kernel state (daemon-owned presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelStateData {
    pub status: KernelStatus,
    pub env_source: String,
}

/// Decoded channel data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "channel", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum ChannelData {
    Cursor(CursorPosition),
    Selection(SelectionRange),
    KernelState(KernelStateData),
    Custom(Vec<u8>),
}

impl ChannelData {
    /// Get the channel identifier for this data.
    pub fn channel(&self) -> Channel {
        match self {
            Self::Cursor(_) => Channel::Cursor,
            Self::Selection(_) => Channel::Selection,
            Self::KernelState(_) => Channel::KernelState,
            Self::Custom(_) => Channel::Custom,
        }
    }
}

// ── Wire messages (CBOR-encoded) ─────────────────────────────────────

/// A presence message as sent over the wire.
///
/// All variants are CBOR-encoded via serde. The `type` tag discriminates
/// the variant, matching automerge-repo's ephemeral message conventions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum PresenceMessage {
    Update {
        peer_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_label: Option<String>,
        #[serde(flatten)]
        data: ChannelData,
    },
    Snapshot {
        /// The sender of this snapshot (typically "daemon").
        /// This is NOT the receiver's peer_id — it identifies who generated
        /// the snapshot so receivers know the source.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSnapshot {
    pub peer_id: String,
    /// Free-form label identifying the peer (e.g. "human", "agent", "daemon").
    pub peer_label: String,
    pub channels: Vec<ChannelData>,
}

// ── Errors ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    #[error("CBOR encode error: {0}")]
    Encode(String),
    #[error("CBOR decode error: {0}")]
    Decode(String),
    #[error("presence frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: usize, max: usize },
}

// ── CBOR encoding ────────────────────────────────────────────────────

/// Encode a presence message as CBOR bytes.
pub fn encode_message(msg: &PresenceMessage) -> Result<Vec<u8>, PresenceError> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(msg, &mut buf).map_err(|e| PresenceError::Encode(e.to_string()))?;
    Ok(buf)
}

/// Decode a presence message from CBOR bytes.
pub fn decode_message(data: &[u8]) -> Result<PresenceMessage, PresenceError> {
    ciborium::de::from_reader(data).map_err(|e| PresenceError::Decode(e.to_string()))
}

// ── Convenience encoders ─────────────────────────────────────────────

/// Encode a cursor update message.
pub fn encode_cursor_update(peer_id: &str, pos: &CursorPosition) -> Vec<u8> {
    encode_cursor_update_labeled(peer_id, None, pos)
}

/// Encode a cursor update with an optional peer label.
pub fn encode_cursor_update_labeled(
    peer_id: &str,
    peer_label: Option<&str>,
    pos: &CursorPosition,
) -> Vec<u8> {
    // Cursor encoding is infallible for valid types — unwrap is safe here.
    encode_message(&PresenceMessage::Update {
        peer_id: peer_id.to_string(),
        peer_label: peer_label.map(|s| s.to_string()),
        data: ChannelData::Cursor(pos.clone()),
    })
    .expect("CBOR encoding of cursor should not fail")
}

/// Encode a selection update message.
pub fn encode_selection_update(peer_id: &str, sel: &SelectionRange) -> Vec<u8> {
    encode_selection_update_labeled(peer_id, None, sel)
}

/// Encode a selection update with an optional peer label.
pub fn encode_selection_update_labeled(
    peer_id: &str,
    peer_label: Option<&str>,
    sel: &SelectionRange,
) -> Vec<u8> {
    encode_message(&PresenceMessage::Update {
        peer_id: peer_id.to_string(),
        peer_label: peer_label.map(|s| s.to_string()),
        data: ChannelData::Selection(sel.clone()),
    })
    .expect("CBOR encoding of selection should not fail")
}

/// Encode a kernel state update message (daemon-owned).
pub fn encode_kernel_state_update(peer_id: &str, state: &KernelStateData) -> Vec<u8> {
    encode_message(&PresenceMessage::Update {
        peer_id: peer_id.to_string(),
        peer_label: None,
        data: ChannelData::KernelState(state.clone()),
    })
    .expect("CBOR encoding of kernel state should not fail")
}

/// Encode a custom channel update message (arbitrary bytes).
///
/// Returns `Err` if encoding fails (e.g. data too large for CBOR).
pub fn encode_custom_update(peer_id: &str, data: &[u8]) -> Result<Vec<u8>, PresenceError> {
    encode_message(&PresenceMessage::Update {
        peer_id: peer_id.to_string(),
        peer_label: None,
        data: ChannelData::Custom(data.to_vec()),
    })
}

/// Encode a heartbeat message.
pub fn encode_heartbeat(peer_id: &str) -> Vec<u8> {
    encode_message(&PresenceMessage::Heartbeat {
        peer_id: peer_id.to_string(),
    })
    .expect("CBOR encoding of heartbeat should not fail")
}

/// Encode a "peer left" message.
pub fn encode_left(peer_id: &str) -> Vec<u8> {
    encode_message(&PresenceMessage::Left {
        peer_id: peer_id.to_string(),
    })
    .expect("CBOR encoding of left should not fail")
}

/// Encode a full snapshot of all peers' presence state.
pub fn encode_snapshot(sender_peer_id: &str, peers: &[PeerSnapshot]) -> Vec<u8> {
    encode_message(&PresenceMessage::Snapshot {
        peer_id: sender_peer_id.to_string(),
        peers: peers.to_vec(),
    })
    .expect("CBOR encoding of snapshot should not fail")
}

/// Validate that a presence frame payload is within the size limit.
///
/// Call this before sending to avoid wasting bandwidth on frames the
/// daemon will drop.
pub fn validate_frame_size(data: &[u8]) -> Result<(), PresenceError> {
    if data.len() > MAX_PRESENCE_FRAME_SIZE {
        Err(PresenceError::FrameTooLarge {
            size: data.len(),
            max: MAX_PRESENCE_FRAME_SIZE,
        })
    } else {
        Ok(())
    }
}

// ── Presence State ───────────────────────────────────────────────────

/// Per-peer presence info tracked by the state manager.
#[derive(Debug, Clone)]
pub struct PeerPresence {
    pub peer_id: String,
    /// Free-form label identifying the peer (e.g. "human", "agent", "daemon").
    pub peer_label: String,
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
    /// The channel is derived from the `data` variant — no separate parameter needed.
    ///
    /// Returns `true` if this is a new peer (caller may want to send a snapshot).
    pub fn update_peer(
        &mut self,
        peer_id: &str,
        peer_label: &str,
        data: ChannelData,
        now_ms: u64,
    ) -> bool {
        let channel = data.channel();
        let is_new = !self.peers.contains_key(peer_id);
        let entry = self
            .peers
            .entry(peer_id.to_string())
            .or_insert_with(|| PeerPresence {
                peer_id: peer_id.to_string(),
                peer_label: peer_label.to_string(),
                channels: HashMap::new(),
                last_seen_ms: now_ms,
                last_active_ms: now_ms,
            });
        entry.channels.insert(channel, data);
        entry.last_seen_ms = now_ms;
        entry.last_active_ms = now_ms;
        entry.peer_label = peer_label.to_string();
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

    /// Build a snapshot of all peers' current state, encoded as CBOR bytes.
    pub fn encode_snapshot(&self, sender_peer_id: &str) -> Vec<u8> {
        let snapshots: Vec<PeerSnapshot> = self
            .peers
            .values()
            .map(|peer| PeerSnapshot {
                peer_id: peer.peer_id.clone(),
                peer_label: peer.peer_label.clone(),
                channels: peer.channels.values().cloned().collect(),
            })
            .collect();
        encode_snapshot(sender_peer_id, &snapshots)
    }

    /// Apply a decoded snapshot, replacing state for all included peers.
    pub fn apply_snapshot(&mut self, peers: &[PeerSnapshot], now_ms: u64) {
        for snap in peers {
            let mut channels = HashMap::new();
            for data in &snap.channels {
                channels.insert(data.channel(), data.clone());
            }
            self.peers.insert(
                snap.peer_id.clone(),
                PeerPresence {
                    peer_id: snap.peer_id.clone(),
                    peer_label: snap.peer_label.clone(),
                    channels,
                    last_seen_ms: now_ms,
                    last_active_ms: now_ms,
                },
            );
        }
    }

    /// Get the kernel state from any peer that has published it.
    pub fn kernel_state(&self) -> Option<&KernelStateData> {
        for peer in self.peers.values() {
            if let Some(ChannelData::KernelState(ks)) = peer.channels.get(&Channel::KernelState) {
                return Some(ks);
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
            PresenceMessage::Update { peer_id, data, .. } => {
                assert_eq!(peer_id, "peer-1");
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
            PresenceMessage::Update { peer_id, data, .. } => {
                assert_eq!(peer_id, "editor-2");
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
            PresenceMessage::Update { peer_id, data, .. } => {
                assert_eq!(peer_id, "daemon");
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
        let peers = vec![
            PeerSnapshot {
                peer_id: "user-1".into(),
                peer_label: "human".into(),
                channels: vec![ChannelData::Cursor(CursorPosition {
                    cell_id: "c1".into(),
                    line: 10,
                    column: 5,
                })],
            },
            PeerSnapshot {
                peer_id: "daemon".into(),
                peer_label: "daemon".into(),
                channels: vec![ChannelData::KernelState(KernelStateData {
                    status: KernelStatus::Busy,
                    env_source: "conda:prewarmed".into(),
                })],
            },
        ];

        let encoded = encode_snapshot("daemon", &peers);
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Snapshot { peer_id, peers } => {
                assert_eq!(peer_id, "daemon");
                assert_eq!(peers.len(), 2);
                assert_eq!(peers[0].peer_id, "user-1");
                assert_eq!(peers[0].peer_label, "human");
                assert_eq!(peers[0].channels.len(), 1);
                assert_eq!(peers[1].peer_id, "daemon");
                assert_eq!(peers[1].peer_label, "daemon");
            }
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn test_custom_channel_roundtrip() {
        let custom = b"{\"foo\":\"bar\"}";
        let encoded = encode_custom_update("agent-x", custom).unwrap();
        let msg = decode_message(&encoded).unwrap();
        match msg {
            PresenceMessage::Update { peer_id, data, .. } => {
                assert_eq!(peer_id, "agent-x");
                assert_eq!(data, ChannelData::Custom(custom.to_vec()));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_validate_frame_size() {
        let small = encode_cursor_update(
            "p1",
            &CursorPosition {
                cell_id: "c1".into(),
                line: 0,
                column: 0,
            },
        );
        assert!(validate_frame_size(&small).is_ok());

        let oversized = vec![0u8; MAX_PRESENCE_FRAME_SIZE + 1];
        assert!(validate_frame_size(&oversized).is_err());
    }

    #[test]
    fn test_cursor_update_is_compact() {
        let pos = CursorPosition {
            cell_id: "c1".into(),
            line: 10,
            column: 5,
        };
        let encoded = encode_cursor_update("p1", &pos);
        // CBOR is slightly larger than hand-rolled but still compact.
        // Exact size depends on CBOR overhead but should be well under 100 bytes.
        assert!(
            encoded.len() < 100,
            "cursor update should be compact, got {} bytes",
            encoded.len()
        );
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
        let is_new = state.update_peer("peer-1", "human", cursor, 1000);
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
        assert!(state.update_peer("peer-1", "human", c1, 1000));
        assert!(!state.update_peer("peer-1", "human", c2, 2000));
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
        state.update_peer("old", "human", cursor.clone(), 1000);
        state.update_peer("recent", "agent", cursor, 50_000);

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
        state.update_peer("peer-1", "human", cursor, 1000);

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
        state.update_peer("peer-1", "human", cursor, 1000);
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
        state.update_peer("daemon", "daemon", ks, 1000);

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
        state.update_peer("me", "human", c1, 1000);
        state.update_peer("agent", "agent", c2, 1000);

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
        state1.update_peer("user", "human", cursor, 1000);
        state1.update_peer("daemon", "daemon", ks, 1000);

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
        state.update_peer("peer-1", "human", cursor, 1000);
        state.update_peer("peer-1", "human", sel, 1000);

        let peer = state.get_peer("peer-1").unwrap();
        assert_eq!(peer.channels.len(), 2);
        assert!(peer.channels.contains_key(&Channel::Cursor));
        assert!(peer.channels.contains_key(&Channel::Selection));
    }

    #[test]
    fn test_channel_data_channel_accessor() {
        let cursor = ChannelData::Cursor(CursorPosition {
            cell_id: "c1".into(),
            line: 0,
            column: 0,
        });
        assert_eq!(cursor.channel(), Channel::Cursor);

        let sel = ChannelData::Selection(SelectionRange {
            cell_id: "c1".into(),
            anchor_line: 0,
            anchor_col: 0,
            head_line: 1,
            head_col: 5,
        });
        assert_eq!(sel.channel(), Channel::Selection);

        let ks = ChannelData::KernelState(KernelStateData {
            status: KernelStatus::Idle,
            env_source: "uv:prewarmed".into(),
        });
        assert_eq!(ks.channel(), Channel::KernelState);

        let custom = ChannelData::Custom(vec![1, 2, 3]);
        assert_eq!(custom.channel(), Channel::Custom);
    }

    #[test]
    fn test_kernel_status_str_roundtrip() {
        for status in [
            KernelStatus::NotStarted,
            KernelStatus::Starting,
            KernelStatus::Idle,
            KernelStatus::Busy,
            KernelStatus::Errored,
            KernelStatus::Shutdown,
        ] {
            let s = status.as_str();
            let recovered = KernelStatus::from_status_str(s);
            assert_eq!(status, recovered, "roundtrip failed for {s}");
        }
    }
}
