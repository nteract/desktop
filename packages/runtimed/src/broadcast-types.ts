/**
 * Typed broadcast interfaces for daemon events.
 *
 * Transport-agnostic versions of the broadcast payloads sent by the daemon
 * via frame type 0x03 (BROADCAST). Provides type guards for filtering
 * the untyped `broadcasts$` observable into typed sub-streams.
 *
 * Path changes, autosave timestamps, and env-preparation progress used to be
 * broadcasts; they are now fields on `RuntimeStateDoc` (`path`, `last_saved`,
 * `env.progress`) and reach clients via normal CRDT sync. Read them via
 * `useRuntimeState()` instead of subscribing to broadcasts.
 */

// ── Broadcast interfaces ────────────────────────────────────────────

export interface CommBroadcast {
  event: "comm";
  msg_type: string;
  content: Record<string, unknown>;
  buffers: number[][];
}

/** Union of all known broadcast types with an `event` field. */
export type KnownBroadcast = CommBroadcast;

// ── Type guards ─────────────────────────────────────────────────────

function hasBroadcastEvent(payload: unknown): payload is { event: string } {
  return (
    typeof payload === "object" &&
    payload !== null &&
    "event" in payload &&
    typeof (payload as { event: unknown }).event === "string"
  );
}

export function isCommBroadcast(payload: unknown): payload is CommBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "comm";
}
