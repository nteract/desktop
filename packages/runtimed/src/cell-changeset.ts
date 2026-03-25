/**
 * CellChangeset types and merge utilities.
 *
 * Pure module with zero external dependencies — safe to import from
 * unit tests without pulling in Tauri, RxJS, or WASM runtime.
 *
 * Mirrors the Rust `notebook_doc::diff` types serialized from WASM
 * via serde-wasm-bindgen.
 */

// ── Types ────────────────────────────────────────────────────────────

/** Which fields changed on a cell (only `true` keys are present). */
export interface ChangedFields {
  source?: boolean;
  outputs?: boolean;
  execution_count?: boolean;
  cell_type?: boolean;
  metadata?: boolean;
  position?: boolean;
  resolved_assets?: boolean;
}

export interface ChangedCell {
  cell_id: string;
  fields: ChangedFields;
}

/** Structural diff between two Automerge head sets, produced by WASM `diff_cells`. */
export interface CellChangeset {
  changed: ChangedCell[];
  added: string[];
  removed: string[];
  order_changed: boolean;
}

// ── Utilities ────────────────────────────────────────────────────────

/**
 * Merge two CellChangesets (for coalescing frames across a buffer window).
 *
 * Field unions are additive — if either changeset marks a field as changed,
 * the merged result marks it as changed. Added/removed lists are deduplicated.
 * `order_changed` is true if either input is true.
 */
export function mergeChangesets(
  a: CellChangeset,
  b: CellChangeset,
): CellChangeset {
  const changedMap = new Map<string, ChangedFields>();
  for (const c of [...a.changed, ...b.changed]) {
    const existing = changedMap.get(c.cell_id);
    if (existing) {
      for (const [key, val] of Object.entries(c.fields)) {
        if (val) (existing as Record<string, boolean>)[key] = true;
      }
    } else {
      changedMap.set(c.cell_id, { ...c.fields });
    }
  }
  return {
    changed: [...changedMap].map(([cell_id, fields]) => ({ cell_id, fields })),
    added: [...new Set([...a.added, ...b.added])],
    removed: [...new Set([...a.removed, ...b.removed])],
    order_changed: a.order_changed || b.order_changed,
  };
}
