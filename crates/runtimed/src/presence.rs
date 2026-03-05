//! Peer presence tracking for notebook rooms.
//!
//! This module manages the presence state of connected peers in a notebook room.
//! Presence includes user display info (name, icon, color) and cursor position.
//! When peers connect or disconnect, other peers are notified via broadcasts.
//!
//! ## Primary Use Case
//!
//! The primary consumers are agents (via runtimed-py and the nteract MCP) that
//! interact with notebooks programmatically. The presence API shows which
//! agents/processes are connected to a notebook.
//!
//! ## Identity Model
//!
//! Each connection gets a unique peer_id (UUID). User display info (name, icon, color)
//! is sent with presence updates. For demos, random animal names are generated.

use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::protocol::PeerPresence;

/// Thread-safe storage for peer presence in a notebook room.
///
/// Tracks all connected peers and their current focus/cursor position.
/// New connections receive the current presence state via PresenceSync broadcast.
pub struct PresenceState {
    /// Active peers: peer_id -> PeerPresence
    peers: RwLock<HashMap<String, PeerPresence>>,
}

impl PresenceState {
    /// Create a new empty presence state.
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Update a peer's presence state.
    ///
    /// Returns `true` if this is a newly connected peer (not seen before).
    pub async fn update(&self, presence: PeerPresence) -> bool {
        let mut peers = self.peers.write().await;
        let is_new = !peers.contains_key(&presence.peer_id);
        peers.insert(presence.peer_id.clone(), presence);
        is_new
    }

    /// Remove a peer (when they disconnect).
    ///
    /// Returns the removed presence if the peer was tracked.
    pub async fn remove(&self, peer_id: &str) -> Option<PeerPresence> {
        let mut peers = self.peers.write().await;
        peers.remove(peer_id)
    }

    /// Get all current peer presence states.
    ///
    /// Used to send PresenceSync to newly connected peers.
    pub async fn get_all(&self) -> Vec<PeerPresence> {
        let peers = self.peers.read().await;
        peers.values().cloned().collect()
    }

    /// Get a specific peer's presence.
    pub async fn get(&self, peer_id: &str) -> Option<PeerPresence> {
        let peers = self.peers.read().await;
        peers.get(peer_id).cloned()
    }

    /// Get the number of connected peers.
    pub async fn count(&self) -> usize {
        let peers = self.peers.read().await;
        peers.len()
    }
}

impl Default for PresenceState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::UserInfo;

    fn make_presence(peer_id: &str, name: &str) -> PeerPresence {
        PeerPresence {
            peer_id: peer_id.to_string(),
            user: UserInfo {
                name: name.to_string(),
                icon: Some("cat".to_string()),
                color: "#FF5733".to_string(),
            },
            cursor: None,
            last_active: 1234567890,
        }
    }

    #[tokio::test]
    async fn test_update_new_peer() {
        let state = PresenceState::new();
        let presence = make_presence("peer-1", "Swift Fox");

        let is_new = state.update(presence.clone()).await;
        assert!(is_new, "first update should report peer as new");

        let is_new = state.update(presence).await;
        assert!(!is_new, "second update should not report peer as new");
    }

    #[tokio::test]
    async fn test_remove_peer() {
        let state = PresenceState::new();
        let presence = make_presence("peer-1", "Swift Fox");

        state.update(presence.clone()).await;
        assert_eq!(state.count().await, 1);

        let removed = state.remove("peer-1").await;
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().peer_id, "peer-1");
        assert_eq!(state.count().await, 0);

        // Removing again should return None
        let removed_again = state.remove("peer-1").await;
        assert!(removed_again.is_none());
    }

    #[tokio::test]
    async fn test_get_all() {
        let state = PresenceState::new();
        state.update(make_presence("peer-1", "Swift Fox")).await;
        state.update(make_presence("peer-2", "Clever Owl")).await;

        let all = state.get_all().await;
        assert_eq!(all.len(), 2);

        let ids: Vec<_> = all.iter().map(|p| p.peer_id.as_str()).collect();
        assert!(ids.contains(&"peer-1"));
        assert!(ids.contains(&"peer-2"));
    }

    #[tokio::test]
    async fn test_get_specific_peer() {
        let state = PresenceState::new();
        state.update(make_presence("peer-1", "Swift Fox")).await;

        let peer = state.get("peer-1").await;
        assert!(peer.is_some());
        assert_eq!(peer.unwrap().user.name, "Swift Fox");

        let missing = state.get("peer-999").await;
        assert!(missing.is_none());
    }
}
