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

// ── Manifest hash detection ─────────────────────────────────────────

const MANIFEST_HASH_RE = /^[a-f0-9]{64}$/;

/**
 * Check if a string looks like a manifest hash (64-char hex SHA-256).
 */
export function isManifestHash(s: string): boolean {
  return MANIFEST_HASH_RE.test(s);
}

export interface OutputManifestHashes {
  hashes: string[];
}

/**
 * Detect unresolved Output widget manifest hashes in comm state.
 *
 * Returns the hashes if `state._model_name === "OutputModel"` and
 * `state.outputs` contains valid manifest hash strings. Returns null
 * if not an OutputModel, outputs are empty, or already resolved.
 */
export function detectOutputManifestHashes(
  state: Record<string, unknown>,
): OutputManifestHashes | null {
  if (state._model_name !== "OutputModel") return null;

  const outputs = state.outputs;
  if (!Array.isArray(outputs) || outputs.length === 0) return null;

  // Check all entries are manifest hash strings
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
