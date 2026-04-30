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

export type CellChangesetMaterialization =
  | { kind: "full"; reason: "missing_changeset" | "structural" | "resolved_assets" }
  | { kind: "incremental" };

export interface IncrementalCellProjection {
  cell_id: string;
  fields: ChangedFields;
  touches_chrome: boolean;
  touches_outputs: boolean;
  preserve_source: boolean;
  field_summary: string[];
}

export type CellChangesetProjectionPlan =
  | { kind: "full"; reason: "missing_changeset" | "structural" | "resolved_assets" }
  | { kind: "incremental"; cells: IncrementalCellProjection[] };

export type CellPointerRefreshPlan =
  | { kind: "all" }
  | { kind: "touched"; cell_ids: string[] }
  | { kind: "none" };

const CHROME_FIELD_KEYS = [
  "source",
  "execution_count",
  "cell_type",
  "metadata",
  "position",
] as const satisfies readonly (keyof ChangedFields)[];

// Debug summaries intentionally mirror the historic frame-pipeline log subset.
const FIELD_SUMMARY_LABELS: ReadonlyArray<[keyof ChangedFields, string]> = [
  ["source", "src"],
  ["outputs", "out"],
  ["execution_count", "ec"],
  ["metadata", "meta"],
];

// ── Utilities ────────────────────────────────────────────────────────

/**
 * Merge two CellChangesets (for coalescing frames across a buffer window).
 *
 * Field unions are additive — if either changeset marks a field as changed,
 * the merged result marks it as changed. Added/removed lists are deduplicated.
 * `order_changed` is true if either input is true.
 */
export function mergeChangesets(a: CellChangeset, b: CellChangeset): CellChangeset {
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

/**
 * Classify how a frontend projection should consume a coalesced changeset.
 *
 * Structural changes and `resolved_assets` updates require a full notebook
 * materialization because per-cell WASM accessors do not expose every value the
 * app-level cell snapshot needs. Pure cell chrome/output updates can use the
 * incremental projection path.
 */
export function classifyCellChangesetMaterialization(
  changeset: CellChangeset | null,
): CellChangesetMaterialization {
  if (!changeset) {
    return { kind: "full", reason: "missing_changeset" };
  }
  if (changeset.added.length > 0 || changeset.removed.length > 0 || changeset.order_changed) {
    return { kind: "full", reason: "structural" };
  }
  if (changeset.changed.some((c) => c.fields.resolved_assets)) {
    return { kind: "full", reason: "resolved_assets" };
  }
  return { kind: "incremental" };
}

export function cellChangesetTouchesChrome(fields: ChangedFields): boolean {
  return CHROME_FIELD_KEYS.some((key) => fields[key] === true);
}

export function summarizeChangedFields(fields: ChangedFields): string[] {
  return FIELD_SUMMARY_LABELS.flatMap(([key, label]) => (fields[key] === true ? [label] : []));
}

/**
 * Plan frontend projection work for a coalesced changeset.
 *
 * This keeps protocol-field interpretation in the runtimed package while
 * allowing apps to supply their own store writes and renderer pre-warming.
 */
export function planCellChangesetProjection(
  changeset: CellChangeset | null,
): CellChangesetProjectionPlan {
  const materialization = classifyCellChangesetMaterialization(changeset);
  if (materialization.kind === "full") {
    return materialization;
  }
  if (!changeset) {
    return { kind: "full", reason: "missing_changeset" };
  }
  return {
    kind: "incremental",
    cells: changeset.changed.map(({ cell_id, fields }) => ({
      cell_id,
      fields,
      touches_chrome: cellChangesetTouchesChrome(fields),
      touches_outputs: fields.outputs === true,
      preserve_source: fields.source !== true,
      field_summary: summarizeChangedFields(fields),
    })),
  };
}

/**
 * Plan which notebook-doc cell execution_id pointers need refreshing after a
 * materialization pass. Incremental non-structural changes only need touched
 * cells; full/structural materialization refreshes the whole document.
 */
export function planCellPointerRefresh(changeset: CellChangeset | null): CellPointerRefreshPlan {
  if (!changeset || changeset.added.length > 0) {
    return { kind: "all" };
  }
  const cellIds = [...new Set(changeset.changed.map((c) => c.cell_id))];
  if (cellIds.length === 0) {
    return { kind: "none" };
  }
  return { kind: "touched", cell_ids: cellIds };
}
