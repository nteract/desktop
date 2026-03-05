/**
 * Hook for presence awareness in notebook rooms.
 *
 * Tracks connected peers (agents, windows) and their focus positions.
 * Primary use case is showing which agents are connected to a notebook
 * via runtimed-py and the nteract MCP.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";
import { generateUserInfo } from "../lib/identity";
import { logger } from "../lib/logger";
import type {
  CursorPosition,
  DaemonBroadcast,
  PeerPresence,
  UserInfo,
} from "../types";

interface UsePresenceOptions {
  /** Throttle presence updates (ms). Default: 100 */
  throttleMs?: number;
}

export function usePresence({ throttleMs = 100 }: UsePresenceOptions = {}) {
  const [peers, setPeers] = useState<Map<string, PeerPresence>>(new Map());
  const [localPeerId, setLocalPeerId] = useState<string | null>(null);

  // Generate user info once per session
  const userInfoRef = useRef<UserInfo | null>(null);
  if (!userInfoRef.current) {
    userInfoRef.current = generateUserInfo();
  }

  // Throttle state for cursor updates
  const lastUpdateRef = useRef<number>(0);
  const pendingCursorRef = useRef<CursorPosition | null>(null);
  const throttleTimerRef = useRef<number | null>(null);

  // Listen for presence broadcasts
  useEffect(() => {
    let cancelled = false;
    const webview = getCurrentWebview();

    const unlistenBroadcast = webview.listen<DaemonBroadcast>(
      "daemon:broadcast",
      (event) => {
        if (cancelled) return;
        const broadcast = event.payload;

        switch (broadcast.event) {
          case "presence_update": {
            const peer = broadcast.peer;
            setPeers((prev) => {
              const next = new Map(prev);
              next.set(peer.peer_id, peer);
              return next;
            });

            // If this is our own presence update, capture our peer_id
            if (
              localPeerId === null &&
              peer.user.name === userInfoRef.current?.name
            ) {
              setLocalPeerId(peer.peer_id);
            }
            break;
          }

          case "peer_disconnected": {
            setPeers((prev) => {
              const next = new Map(prev);
              next.delete(broadcast.peer_id);
              return next;
            });
            break;
          }

          case "presence_sync": {
            // Initial sync of all peers when we connect
            const peerMap = new Map<string, PeerPresence>();
            for (const peer of broadcast.peers) {
              peerMap.set(peer.peer_id, peer);
            }
            setPeers(peerMap);
            logger.info(`[presence] Synced ${broadcast.peers.length} peers`);
            break;
          }
        }
      },
    );

    return () => {
      cancelled = true;
      if (throttleTimerRef.current) {
        clearTimeout(throttleTimerRef.current);
      }
      unlistenBroadcast.then((fn) => fn()).catch(() => {});
    };
  }, [localPeerId]);

  // Send presence update to daemon
  const sendPresenceUpdate = useCallback(
    (cursor: CursorPosition | null) => {
      const now = Date.now();
      const elapsed = now - lastUpdateRef.current;

      const doUpdate = () => {
        lastUpdateRef.current = Date.now();
        invoke("update_presence", {
          user: userInfoRef.current,
          cursor,
        }).catch((e) => logger.error("[presence] Update failed:", e));
      };

      if (elapsed >= throttleMs) {
        // Send immediately
        doUpdate();
      } else {
        // Throttle: schedule for later
        pendingCursorRef.current = cursor;
        if (!throttleTimerRef.current) {
          throttleTimerRef.current = window.setTimeout(() => {
            throttleTimerRef.current = null;
            invoke("update_presence", {
              user: userInfoRef.current,
              cursor: pendingCursorRef.current,
            }).catch((e) => logger.error("[presence] Update failed:", e));
          }, throttleMs - elapsed);
        }
      }
    },
    [throttleMs],
  );

  // Update cursor position (call when user focuses a cell)
  const updateCursor = useCallback(
    (cellId: string, offset?: number, selectionEnd?: number) => {
      sendPresenceUpdate({
        cell_id: cellId,
        offset,
        selection_end: selectionEnd,
      });
    },
    [sendPresenceUpdate],
  );

  // Clear cursor (e.g., when window loses focus)
  const clearCursor = useCallback(() => {
    sendPresenceUpdate(null);
  }, [sendPresenceUpdate]);

  // Filter out local peer for display
  const otherPeers = Array.from(peers.values()).filter(
    (p) => p.peer_id !== localPeerId,
  );

  // Get peers focused on a specific cell
  const getPeersInCell = useCallback(
    (cellId: string) => {
      return otherPeers.filter((p) => p.cursor?.cell_id === cellId);
    },
    [otherPeers],
  );

  return {
    /** All other peers' presence (excludes self) */
    peers: otherPeers,
    /** All peers including self */
    allPeers: Array.from(peers.values()),
    /** Local peer ID (null until first presence update) */
    localPeerId,
    /** Local user info */
    userInfo: userInfoRef.current,
    /** Update cursor position */
    updateCursor,
    /** Clear cursor (window blur) */
    clearCursor,
    /** Get peers focused on a specific cell */
    getPeersInCell,
  };
}
