import { useCallback } from "react";
import { frame_types, sendFrame } from "../lib/frame-types";
import { logger } from "../lib/logger";
import {
  encode_cursor_presence,
  encode_focus_presence,
  encode_selection_presence,
} from "../wasm/runtimed-wasm/runtimed_wasm.js";

// ── Hook ─────────────────────────────────────────────────────────────

/**
 * Send-only presence hook. Provides methods to broadcast local cursor
 * and selection positions to remote peers via the daemon.
 *
 * Read-side presence (remote cursors, selections, peer tracking) is
 * handled by cursor-registry.ts — a module-level frame bus subscriber
 * that dispatches directly to CodeMirror without React involvement.
 *
 * @param peerId The local peer's ID. When `null`, the hook is inactive.
 * @param peerLabel Human-readable label for this peer (e.g. OS username).
 * @param actorLabel Automerge actor label for CRDT attribution identity.
 */
export function usePresence(
  peerId: string | null,
  peerLabel: string = "",
  actorLabel: string = "",
) {
  const setCursor = useCallback(
    (cellId: string, line: number, column: number) => {
      if (!peerId) return;
      const payload = encode_cursor_presence(peerId, peerLabel, actorLabel, cellId, line, column);
      sendFrame(frame_types.PRESENCE, payload).catch((e: unknown) =>
        logger.warn("[presence] send cursor failed:", e),
      );
    },
    [peerId, peerLabel, actorLabel],
  );

  const setSelection = useCallback(
    (cellId: string, anchorLine: number, anchorCol: number, headLine: number, headCol: number) => {
      if (!peerId) return;
      const payload = encode_selection_presence(
        peerId,
        peerLabel,
        actorLabel,
        cellId,
        anchorLine,
        anchorCol,
        headLine,
        headCol,
      );
      sendFrame(frame_types.PRESENCE, payload).catch((e: unknown) =>
        logger.warn("[presence] send selection failed:", e),
      );
    },
    [peerId, peerLabel, actorLabel],
  );

  const setFocus = useCallback(
    (cellId: string) => {
      if (!peerId) return;
      const payload = encode_focus_presence(peerId, peerLabel, actorLabel, cellId);
      sendFrame(frame_types.PRESENCE, payload).catch((e: unknown) =>
        logger.warn("[presence] send focus failed:", e),
      );
    },
    [peerId, peerLabel, actorLabel],
  );

  return {
    /** Set the local cursor position (fire-and-forget). */
    setCursor,
    /** Set the local selection range (fire-and-forget). */
    setSelection,
    /** Set cell-level focus presence (no cursor position). */
    setFocus,
  };
}
