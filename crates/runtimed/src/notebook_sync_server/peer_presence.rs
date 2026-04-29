use std::sync::Arc;

use notebook_doc::presence::{self, ChannelData, PresenceMessage};
use tokio::io::AsyncWrite;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::connection::{self, NotebookFrameType};

use super::peer_writer::PeerWriter;
use super::NotebookRoom;

/// Sanitize a peer label from the wire.
///
/// - Strips zero-width and control characters (ZWJ, ZWNJ, ZWSP, etc.)
/// - Trims whitespace
/// - Clamps to 64 Unicode scalar values
/// - Falls back to `fallback` if empty/missing
pub(crate) fn sanitize_peer_label(raw: Option<&str>, fallback: &str) -> String {
    const MAX_LABEL_CHARS: usize = 64;

    fn is_allowed(c: char) -> bool {
        !c.is_control()
            && !matches!(
                c,
                '\u{200B}' // zero-width space
                | '\u{200C}' // zero-width non-joiner
                | '\u{200D}' // zero-width joiner
                | '\u{200E}' // left-to-right mark
                | '\u{200F}' // right-to-left mark
                | '\u{2060}' // word joiner
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{00AD}' // soft hyphen
                | '\u{034F}' // combining grapheme joiner
                | '\u{061C}' // arabic letter mark
                | '\u{115F}' // hangul choseong filler
                | '\u{1160}' // hangul jungseong filler
                | '\u{17B4}' // khmer vowel inherent aq
                | '\u{17B5}' // khmer vowel inherent aa
                | '\u{180E}' // mongolian vowel separator
            )
            && !('\u{2066}'..='\u{2069}').contains(&c) // bidi isolates
            && !('\u{202A}'..='\u{202E}').contains(&c) // bidi overrides
            && !('\u{FE00}'..='\u{FE0F}').contains(&c) // variation selectors
            && !('\u{E0100}'..='\u{E01EF}').contains(&c) // variation selectors supplement
    }

    match raw {
        Some(s) => {
            // Filter and take at most MAX_LABEL_CHARS in one pass — avoids
            // allocating proportional to attacker-controlled input size.
            let cleaned: String = s
                .trim()
                .chars()
                .filter(|c| is_allowed(*c))
                .take(MAX_LABEL_CHARS)
                .collect();
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                fallback.to_string()
            } else {
                trimmed.to_string()
            }
        }
        None => fallback.to_string(),
    }
}

pub(super) async fn cleanup_presence_on_disconnect(room: &Arc<NotebookRoom>, peer_id: &str) {
    room.broadcasts.presence.write().await.remove_peer(peer_id);
    match presence::encode_left(peer_id) {
        Ok(left_bytes) => {
            let _ = room
                .broadcasts
                .presence_tx
                .send((peer_id.to_string(), left_bytes));
        }
        Err(e) => warn!("[notebook-sync] Failed to encode 'left' presence: {}", e),
    }
}

pub(super) async fn send_initial_presence_snapshot<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    peer_id: &str,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let snapshot_bytes = {
        let presence_state = room.broadcasts.presence.read().await;
        if presence_state.peer_count() == 0 {
            None
        } else {
            // Build snapshot excluding this peer; clients should not render
            // their own server-assigned peer id as a remote cursor.
            let other_peers: Vec<presence::PeerSnapshot> = presence_state
                .peers()
                .values()
                .filter(|p| p.peer_id != peer_id)
                .map(|p| presence::PeerSnapshot {
                    peer_id: p.peer_id.clone(),
                    peer_label: p.peer_label.clone(),
                    actor_label: p.actor_label.clone(),
                    channels: p.channels.values().cloned().collect(),
                })
                .collect();
            if other_peers.is_empty() {
                None
            } else {
                match presence::encode_snapshot("daemon", &other_peers) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        warn!("[notebook-sync] Failed to encode presence snapshot: {}", e);
                        None
                    }
                }
            }
        }
    };

    if let Some(snapshot_bytes) = snapshot_bytes {
        connection::send_typed_frame(writer, NotebookFrameType::Presence, &snapshot_bytes).await?;
    }
    Ok(())
}

pub(super) async fn handle_presence_frame(
    room: &Arc<NotebookRoom>,
    peer_id: &str,
    peer_writer: &PeerWriter,
    payload: &[u8],
) -> anyhow::Result<()> {
    if payload.len() > presence::MAX_PRESENCE_FRAME_SIZE {
        warn!(
            "[notebook-sync] Oversized presence frame ({} bytes, max {}), dropping",
            payload.len(),
            presence::MAX_PRESENCE_FRAME_SIZE
        );
        return Ok(());
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    match presence::decode_message(payload) {
        Ok(PresenceMessage::Update {
            data,
            peer_label,
            actor_label,
            ..
        }) => {
            // Reject daemon-owned channels before updating shared state.
            // This prevents clients from spoofing kernel status.
            if matches!(data, ChannelData::KernelState(_)) {
                warn!("[notebook-sync] Client tried to publish KernelState presence, ignoring");
                return Ok(());
            }

            let data_for_relay = data.clone();
            let actor_label_for_relay = actor_label.clone();
            let label = sanitize_peer_label(peer_label.as_deref(), peer_id);
            let sanitized_label = Some(label.clone());

            let is_new = room.broadcasts.presence.write().await.update_peer(
                peer_id,
                &label,
                actor_label.as_deref(),
                data,
                now_ms,
            );

            if is_new {
                send_new_peer_snapshot(room, peer_id, peer_writer).await?;
            }

            if let Ok(bytes) = presence::encode_message(&PresenceMessage::Update {
                peer_id: peer_id.to_string(),
                peer_label: sanitized_label,
                actor_label: actor_label_for_relay,
                data: data_for_relay,
            }) {
                let _ = room
                    .broadcasts
                    .presence_tx
                    .send((peer_id.to_string(), bytes));
            }
        }
        Ok(PresenceMessage::Heartbeat { .. }) => {
            room.broadcasts
                .presence
                .write()
                .await
                .mark_seen(peer_id, now_ms);
        }
        Ok(PresenceMessage::ClearChannel { channel, .. }) => {
            room.broadcasts
                .presence
                .write()
                .await
                .clear_channel(peer_id, channel);
            match presence::encode_clear_channel(peer_id, channel) {
                Ok(bytes) => {
                    let _ = room
                        .broadcasts
                        .presence_tx
                        .send((peer_id.to_string(), bytes));
                }
                Err(e) => warn!(
                    "[notebook-sync] Failed to encode clear_channel presence: {}",
                    e
                ),
            }
        }
        Ok(_) => {
            // Snapshot/Left from a client — ignore.
        }
        Err(e) => {
            warn!("[notebook-sync] Failed to decode presence frame: {}", e);
        }
    }
    Ok(())
}

async fn send_new_peer_snapshot(
    room: &Arc<NotebookRoom>,
    peer_id: &str,
    peer_writer: &PeerWriter,
) -> anyhow::Result<()> {
    let other_peers: Vec<presence::PeerSnapshot> = room
        .broadcasts
        .presence
        .read()
        .await
        .peers()
        .values()
        .filter(|p| p.peer_id != peer_id)
        .map(|p| presence::PeerSnapshot {
            peer_id: p.peer_id.clone(),
            peer_label: p.peer_label.clone(),
            actor_label: p.actor_label.clone(),
            channels: p.channels.values().cloned().collect(),
        })
        .collect();

    if !other_peers.is_empty() {
        match presence::encode_snapshot("daemon", &other_peers) {
            Ok(snapshot_bytes) => {
                peer_writer.send_frame(NotebookFrameType::Presence, snapshot_bytes)?;
            }
            Err(e) => warn!(
                "[notebook-sync] Failed to encode presence snapshot for new peer: {}",
                e
            ),
        }
    }
    Ok(())
}

pub(super) async fn forward_presence_broadcast(
    room: &Arc<NotebookRoom>,
    peer_id: &str,
    peer_writer: &PeerWriter,
    result: Result<(String, Vec<u8>), broadcast::error::RecvError>,
) -> anyhow::Result<bool> {
    match result {
        Ok((ref sender_peer_id, ref bytes)) => {
            // Don't echo back to the sender.
            if sender_peer_id != peer_id {
                peer_writer.send_frame(NotebookFrameType::Presence, bytes.clone())?;
            }
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            // Missed some presence updates — send a full snapshot to catch up.
            debug!(
                "[notebook-sync] Peer {} lagged {} presence updates, sending snapshot",
                peer_id, n
            );
            match room
                .broadcasts
                .presence
                .read()
                .await
                .encode_snapshot(peer_id)
            {
                Ok(snapshot_bytes) => {
                    peer_writer.send_frame(NotebookFrameType::Presence, snapshot_bytes)?;
                }
                Err(e) => warn!(
                    "[notebook-sync] Failed to encode lag-recovery snapshot: {}",
                    e
                ),
            }
        }
        Err(broadcast::error::RecvError::Closed) => {
            // Presence channel closed — room is being evicted.
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn prune_stale_presence(room: &Arc<NotebookRoom>, peer_id: &str) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut presence_state = room.broadcasts.presence.write().await;
    presence_state.mark_seen(peer_id, now_ms);
    let pruned = presence_state.prune_stale(now_ms, presence::DEFAULT_PEER_TTL_MS);
    drop(presence_state);

    for pruned_peer_id in pruned {
        match presence::encode_left(&pruned_peer_id) {
            Ok(left_bytes) => {
                let _ = room
                    .broadcasts
                    .presence_tx
                    .send((pruned_peer_id, left_bytes));
            }
            Err(e) => warn!(
                "[notebook-sync] Failed to encode 'left' for pruned peer: {}",
                e
            ),
        }
    }
}
