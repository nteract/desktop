/**
 * Typed broadcast interfaces for daemon events.
 *
 * Transport-agnostic versions of the broadcast payloads sent by the daemon
 * via frame type 0x03 (BROADCAST). Provides type guards for filtering
 * the untyped `broadcasts$` observable into typed sub-streams.
 */

import type { RuntimeState } from "./runtime-state";

// ── Broadcast interfaces ────────────────────────────────────────────

export interface OutputBroadcast {
  event: "output";
  cell_id: string;
  execution_id: string;
  output_type: string;
  output_json: string;
}

export interface DisplayUpdateBroadcast {
  event: "display_update";
  display_id: string;
  data: Record<string, unknown>;
  metadata: Record<string, unknown>;
}

export interface OutputsClearedBroadcast {
  event: "outputs_cleared";
  cell_id: string;
}

export interface CommBroadcast {
  event: "comm";
  msg_type: string;
  content: Record<string, unknown>;
  buffers: number[][];
}

export interface KernelErrorBroadcast {
  event: "kernel_error";
  error: string;
}

export interface RuntimeStateSnapshotBroadcast {
  event: "runtime_state_snapshot";
  state: RuntimeState;
}

/** Union of all known broadcast types with an `event` field. */
export type KnownBroadcast =
  | OutputBroadcast
  | DisplayUpdateBroadcast
  | OutputsClearedBroadcast
  | CommBroadcast
  | KernelErrorBroadcast
  | RuntimeStateSnapshotBroadcast;

// ── Type guards ─────────────────────────────────────────────────────

function hasBroadcastEvent(payload: unknown): payload is { event: string } {
  return (
    typeof payload === "object" &&
    payload !== null &&
    "event" in payload &&
    typeof (payload as { event: unknown }).event === "string"
  );
}

export function isOutputBroadcast(payload: unknown): payload is OutputBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "output";
}

export function isDisplayUpdateBroadcast(payload: unknown): payload is DisplayUpdateBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "display_update";
}

export function isOutputsClearedBroadcast(payload: unknown): payload is OutputsClearedBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "outputs_cleared";
}

export function isCommBroadcast(payload: unknown): payload is CommBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "comm";
}

export function isKernelErrorBroadcast(payload: unknown): payload is KernelErrorBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "kernel_error";
}

export function isRuntimeStateSnapshotBroadcast(
  payload: unknown,
): payload is RuntimeStateSnapshotBroadcast {
  return hasBroadcastEvent(payload) && payload.event === "runtime_state_snapshot";
}
