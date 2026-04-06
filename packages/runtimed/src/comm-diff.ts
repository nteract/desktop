/**
 * Comm state diffing — pure logic for detecting widget lifecycle changes.
 *
 * Diffs previous vs current CRDT comm state to detect:
 * - New comms (comm_open) — sorted by seq for dependency order
 * - State changes (comm_msg update)
 * - Removed comms (comm_close)
 *
 * No blob resolution, no JupyterMessage synthesis — callers handle
 * platform-specific concerns.
 */

import type { CommDocEntry } from "./runtime-state";

// ── Types ───────────────────────────────────────────────────────────

export interface CommDiffResult {
  /** New comms, sorted by seq for correct widget dependency order. */
  opened: Array<{ commId: string; entry: CommDocEntry }>;
  /** Comms whose state changed (JSON-level diff). */
  updated: Array<{ commId: string; entry: CommDocEntry }>;
  /** Comm IDs that were removed. */
  closed: string[];
}

export interface CommDiffState {
  comms: Record<string, CommDocEntry>;
  json: Record<string, string>;
}

// ── Resolved comm types (for SyncEngine.commChanges$) ──────────────

/** A comm with metadata + ContentRef-resolved state, ready for a widget store. */
export interface ResolvedComm {
  commId: string;
  targetName: string;
  modelModule: string;
  modelName: string;
  /** State with blob ContentRefs replaced by URL strings, inlines unwrapped. */
  state: Record<string, unknown>;
  /** JSON paths where blob URLs replaced ContentRef objects (for binary fetch). */
  bufferPaths: string[][];
  /** Unresolved outputs for OutputModel widgets (null if not an OutputModel). */
  unresolvedOutputs: unknown[] | null;
}

/** Comm lifecycle changes emitted by SyncEngine.commChanges$. */
export interface CommChanges {
  /** New comms (sorted by seq for dependency order), with resolved state. */
  opened: ResolvedComm[];
  /** Comms whose state changed, with resolved state. */
  updated: ResolvedComm[];
  /** Comm IDs that were removed. */
  closed: string[];
}

// ── Output manifest detection ───────────────────────────────────────

const MANIFEST_HASH_RE = /^[a-f0-9]{64}$/;

/**
 * Check if a string looks like a manifest hash (64-char hex SHA-256).
 */
export function isManifestHash(s: string): boolean {
  return MANIFEST_HASH_RE.test(s);
}

/** @deprecated Use {@link UnresolvedOutputs} instead. */
export interface OutputManifestHashes {
  hashes: string[];
}

export interface UnresolvedOutputs {
  outputs: unknown[];
}

/**
 * Check whether `value` looks like an inline manifest object
 * (has an `output_type` string property).
 */
function isManifestObject(value: unknown): boolean {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as Record<string, unknown>).output_type === "string"
  );
}

/**
 * Detect unresolved Output widget outputs in comm state.
 *
 * Returns the outputs if `state._model_name === "OutputModel"` and
 * `state.outputs` contains entries that are either manifest hash strings
 * (64-char hex SHA-256) or inline manifest objects (with `output_type`).
 * Returns null if not an OutputModel, outputs are empty, or already resolved.
 */
export function detectUnresolvedOutputs(
  state: Record<string, unknown>,
): UnresolvedOutputs | null {
  if (state._model_name !== "OutputModel") return null;

  const outputs = state.outputs;
  if (!Array.isArray(outputs) || outputs.length === 0) return null;

  // Every entry must be either a manifest hash string or a manifest object
  const allUnresolved = outputs.every(
    (o) => (typeof o === "string" && isManifestHash(o)) || isManifestObject(o),
  );
  if (!allUnresolved) return null;

  return { outputs: outputs as unknown[] };
}

/**
 * @deprecated Use {@link detectUnresolvedOutputs} instead.
 */
export function detectOutputManifestHashes(
  state: Record<string, unknown>,
): OutputManifestHashes | null {
  if (state._model_name !== "OutputModel") return null;

  const outputs = state.outputs;
  if (!Array.isArray(outputs) || outputs.length === 0) return null;

  const allHashes = outputs.every(
    (o) => typeof o === "string" && isManifestHash(o),
  );
  if (!allHashes) return null;

  return { hashes: outputs as string[] };
}

// ── Diff function ───────────────────────────────────────────────────

/**
 * Diff previous and current comm state from RuntimeStateDoc.
 *
 * Returns the structural diff (opened, updated, closed) and the next
 * tracking state for the caller to store. The caller decides which
 * opened/updated comms to include in tracking (e.g., skip comms
 * that couldn't be delivered due to missing blob port).
 *
 * @param prev - Previous tracking state
 * @param curr - Current comms from RuntimeState
 */
export function diffComms(
  prev: CommDiffState,
  curr: Record<string, CommDocEntry>,
): { result: CommDiffResult; next: CommDiffState } {
  const opened: CommDiffResult["opened"] = [];
  const updated: CommDiffResult["updated"] = [];
  const closed: string[] = [];

  const nextComms: Record<string, CommDocEntry> = {};
  const nextJson: Record<string, string> = {};

  // New comms — sorted by seq for dependency order
  const newEntries = Object.entries(curr)
    .filter(([commId]) => !(commId in prev.comms))
    .sort(([, a], [, b]) => (a.seq ?? 0) - (b.seq ?? 0));

  for (const [commId, entry] of newEntries) {
    opened.push({ commId, entry });
    nextComms[commId] = entry;
    nextJson[commId] = JSON.stringify(entry.state);
  }

  // State changes for existing comms
  for (const [commId, entry] of Object.entries(curr)) {
    const stateStr = JSON.stringify(entry.state);
    if (commId in prev.comms) {
      nextComms[commId] = entry;
      nextJson[commId] = stateStr;
      if (prev.json[commId] !== stateStr) {
        updated.push({ commId, entry });
      }
    }
  }

  // Removed comms
  for (const commId of Object.keys(prev.comms)) {
    if (!curr[commId]) {
      closed.push(commId);
    }
  }

  return {
    result: { opened, updated, closed },
    next: { comms: nextComms, json: nextJson },
  };
}
