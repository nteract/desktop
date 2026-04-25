/**
 * Typed broadcast interfaces for daemon events.
 *
 * Transport-agnostic versions of the broadcast payloads sent by the daemon
 * via frame type 0x03 (BROADCAST). Provides type guards for filtering
 * the untyped `broadcasts$` observable into typed sub-streams.
 *
 * Path changes and autosave timestamps used to be broadcasts; they are now
 * fields on `RuntimeStateDoc` (`path`, `last_saved`) and reach clients via
 * normal CRDT sync. Read them via `useRuntimeState()` instead of subscribing
 * to broadcasts.
 */

// ── Broadcast interfaces ────────────────────────────────────────────

/**
 * Reference to a binary blob in the daemon's blob store. Mirrors the
 * Rust `BlobRef` struct in `notebook-protocol`. Consumers fetch the
 * bytes via `GET http://127.0.0.1:<blob_port>/blob/<hash>`.
 */
export interface BlobRef {
  blob: string;
  size: number;
  media_type: string;
}

export interface CommBroadcast {
  event: "comm";
  msg_type: string;
  content: Record<string, unknown>;
  /**
   * Widget binary buffers, offloaded to the blob store. Empty for
   * messages that carry no buffers.
   */
  buffers: BlobRef[];
}

export interface EnvProgressBroadcast {
  event: "env_progress";
  env_type: string;
  // Phase-specific fields are flattened on the wire; carry them through
  // as a permissive index so the daemon can extend the shape without
  // breaking subscribers.
  [key: string]: unknown;
}

/** Union of all known broadcast types with an `event` field. */
export type KnownBroadcast = CommBroadcast | EnvProgressBroadcast;

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

export function isEnvProgressBroadcast(payload: unknown): payload is EnvProgressBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "env_progress";
}
