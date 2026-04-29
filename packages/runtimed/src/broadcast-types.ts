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

export interface CommBroadcast {
  event: "comm";
  msg_type: string;
  content: Record<string, unknown>;
  buffers: number[][];
}

export type EnvProgressEnvType = "conda" | "uv" | "pixi" | (string & {});

export type EnvProgressPhase =
  | { phase: "starting"; env_hash: string }
  | { phase: "cache_hit"; env_path: string }
  | { phase: "lock_file_hit" }
  | { phase: "offline_hit" }
  | { phase: "fetching_repodata"; channels: string[] }
  | { phase: "repodata_complete"; record_count: number; elapsed_ms: number }
  | { phase: "solving"; spec_count: number }
  | { phase: "solve_complete"; package_count: number; elapsed_ms: number }
  | { phase: "installing"; total: number }
  | {
      phase: "download_progress";
      completed: number;
      total: number;
      current_package: string;
      bytes_downloaded: number;
      bytes_total: number | null;
      bytes_per_second: number;
    }
  | {
      phase: "link_progress";
      completed: number;
      total: number;
      current_package: string;
    }
  | { phase: "install_complete"; elapsed_ms: number }
  | { phase: "creating_venv" }
  | { phase: "installing_packages"; packages: string[] }
  | { phase: "ready"; env_path: string; python_path: string }
  | { phase: "error"; message: string };

export type EnvProgressEvent = EnvProgressPhase & {
  env_type: EnvProgressEnvType;
};

export type EnvProgressBroadcast = EnvProgressEvent & {
  event: "env_progress";
};

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
