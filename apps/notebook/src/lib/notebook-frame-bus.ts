/**
 * Module-level pub/sub for notebook frame events.
 *
 * Replaces the webview.emit fan-out pattern where `useAutomergeNotebook`
 * re-emitted `notebook:broadcast` and `notebook:presence` as Tauri webview
 * events. Subscribers now receive payloads via direct in-memory dispatch —
 * synchronous, typed, no event loop hop.
 *
 * The `notebook:frame` Tauri listener stays in `useAutomergeNotebook` (it
 * owns the WASM handle for sync demux). After WASM `receive_frame()` returns
 * typed events, this bus dispatches broadcast and presence payloads to
 * subscribers without a webview round-trip.
 */

// ── Types ────────────────────────────────────────────────────────────

/** Callback for broadcast events (kernel status, output, env progress, etc.) */
export type BroadcastSubscriber = (payload: unknown) => void;

/** Callback for presence events (cursor, selection, snapshot, left, heartbeat) */
export type PresenceSubscriber = (payload: unknown) => void;

// ── State ────────────────────────────────────────────────────────────

const broadcastSubscribers = new Set<BroadcastSubscriber>();
const presenceSubscribers = new Set<PresenceSubscriber>();

// ── Subscribe ────────────────────────────────────────────────────────

/**
 * Subscribe to broadcast events. Returns an unsubscribe function.
 *
 * Broadcasts include kernel_status, KernelLaunched, env_progress,
 * file_changed, and other daemon lifecycle events scoped to the notebook room.
 */
export function subscribeBroadcast(cb: BroadcastSubscriber): () => void {
  broadcastSubscribers.add(cb);
  return () => {
    broadcastSubscribers.delete(cb);
  };
}

/**
 * Subscribe to presence events. Returns an unsubscribe function.
 *
 * Presence messages include update (cursor/selection), snapshot,
 * left, and heartbeat — decoded from CBOR by WASM.
 */
export function subscribePresence(cb: PresenceSubscriber): () => void {
  presenceSubscribers.add(cb);
  return () => {
    presenceSubscribers.delete(cb);
  };
}

// ── Emit (called by useAutomergeNotebook after WASM demux) ───────────

/**
 * Dispatch a broadcast payload to all subscribers.
 *
 * Called by `useAutomergeNotebook` when WASM `receive_frame()` returns
 * a `broadcast` event. The payload is already a parsed JS object.
 */
export function emitBroadcast(payload: unknown): void {
  for (const cb of broadcastSubscribers) {
    try {
      cb(payload);
    } catch {
      // Subscriber errors must not break the dispatch loop
    }
  }
}

/**
 * Dispatch a presence payload to all subscribers.
 *
 * Called by `useAutomergeNotebook` when WASM `receive_frame()` returns
 * a `presence` event. The payload is a decoded PresenceMessage object.
 */
export function emitPresence(payload: unknown): void {
  for (const cb of presenceSubscribers) {
    try {
      cb(payload);
    } catch {
      // Subscriber errors must not break the dispatch loop
    }
  }
}
