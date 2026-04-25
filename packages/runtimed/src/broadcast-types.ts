/**
 * Typed broadcast interfaces for daemon events.
 *
 * Transport-agnostic versions of the broadcast payloads sent by the daemon
 * via frame type 0x03 (BROADCAST). Provides type guards for filtering
 * the untyped `broadcasts$` observable into typed sub-streams.
 */

// ── Broadcast interfaces ────────────────────────────────────────────

export interface CommBroadcast {
  event: "comm";
  msg_type: string;
  content: Record<string, unknown>;
  buffers: number[][];
}

export interface EnvProgressBroadcast {
  event: "env_progress";
  env_type: string;
  // Phase-specific fields are flattened on the wire; carry them through
  // as a permissive index so the daemon can extend the shape without
  // breaking subscribers.
  [key: string]: unknown;
}

export interface PathChangedBroadcast {
  event: "path_changed";
  path: string | null;
}

export interface NotebookAutosavedBroadcast {
  event: "notebook_autosaved";
  path: string;
}

/** Union of all known broadcast types with an `event` field. */
export type KnownBroadcast =
  | CommBroadcast
  | EnvProgressBroadcast
  | PathChangedBroadcast
  | NotebookAutosavedBroadcast;

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

export function isPathChangedBroadcast(payload: unknown): payload is PathChangedBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "path_changed";
}

export function isNotebookAutosavedBroadcast(
  payload: unknown,
): payload is NotebookAutosavedBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "notebook_autosaved";
}
