/**
 * Cell height cache — pretext-based height estimation for virtualized rendering.
 *
 * Uses @chenglou/pretext to measure code cell source heights without DOM.
 * Output heights are estimated by type until the cell renders, at which point
 * a ResizeObserver measurement replaces the estimate.
 *
 * Same useSyncExternalStore pattern as notebook-cells.ts.
 */

import { layout, type PreparedText, prepare } from "@chenglou/pretext";
import type { JupyterOutput, NotebookCell } from "../types";

// ── Constants ──────────────────────────────────────────────────────────

/** Font shorthand matching the CodeMirror editor CSS. */
const CODE_FONT = '14px "SF Mono", Consolas, Monaco, "Andale Mono", monospace';

/** CodeMirror default line height factor. */
const CODE_LINE_HEIGHT = 19.6; // 14px × 1.4

/** Estimated cell chrome: gutter, padding, adder row between cells. */
const DEFAULT_CHROME_HEIGHT = 84;

/** Markdown cell chrome (no output area). */
const MARKDOWN_CHROME_HEIGHT = 56;

// ── Output height estimation defaults ──────────────────────────────────

const OUTPUT_HEIGHT_DEFAULTS: Record<string, number> = {
  "text/html": 200,
  "image/png": 300,
  "image/jpeg": 300,
  "image/svg+xml": 250,
  "image/gif": 200,
  "application/vnd.vegalite.v5+json": 350,
  "application/vnd.vegalite.v4+json": 350,
  "application/vnd.vegalite.v3+json": 350,
  "application/vnd.plotly.v1+json": 400,
  "application/pdf": 500,
  "text/markdown": 100,
  "text/latex": 80,
  "application/vnd.jupyter.widget-view+json": 200,
};
const DEFAULT_OUTPUT_ESTIMATE = 150;

// ── Per-cell entry ─────────────────────────────────────────────────────

interface CellHeightEntry {
  sourceHeight: number;
  outputHeight: number;
  chromeHeight: number;
  totalHeight: number;
  /** Cached pretext PreparedText handle. null for markdown/raw with no source. */
  prepared: PreparedText | null;
  /** The source string that was prepared (for change detection). */
  preparedSource: string;
  /** Whether outputHeight came from a DOM measurement vs estimation. */
  outputMeasured: boolean;
}

// ── State ──────────────────────────────────────────────────────────────

const _entries = new Map<string, CellHeightEntry>();
let _containerWidth = 600; // reasonable initial guess
let _version = 0;
const _subscribers = new Set<() => void>();

function emit(): void {
  _version++;
  for (const cb of _subscribers) cb();
}

// ── Public subscription API ────────────────────────────────────────────

export function subscribeHeightChanges(cb: () => void): () => void {
  _subscribers.add(cb);
  return () => _subscribers.delete(cb);
}

export function getHeightVersion(): number {
  return _version;
}

// ── Height queries ─────────────────────────────────────────────────────

/** Get the total height of all cells in order. */
export function getTotalHeight(cellIds: string[]): number {
  let total = 0;
  for (const id of cellIds) {
    total += _entries.get(id)?.totalHeight ?? DEFAULT_CHROME_HEIGHT;
  }
  return total;
}

/**
 * Build cumulative offsets array for binary search.
 * offsets[i] = top position of cell i (sum of heights of cells 0..i-1).
 * offsets[cellIds.length] = total height.
 */
export function getCumulativeOffsets(cellIds: string[]): Float64Array {
  const offsets = new Float64Array(cellIds.length + 1);
  let y = 0;
  for (let i = 0; i < cellIds.length; i++) {
    offsets[i] = y;
    y += _entries.get(cellIds[i])?.totalHeight ?? DEFAULT_CHROME_HEIGHT;
  }
  offsets[cellIds.length] = y;
  return offsets;
}

/** Get height of a single cell. */
export function getCellHeight(cellId: string): number {
  return _entries.get(cellId)?.totalHeight ?? DEFAULT_CHROME_HEIGHT;
}

// ── Preparation ────────────────────────────────────────────────────────

function prepareSource(source: string): PreparedText | null {
  if (!source) return null;
  return prepare(source, CODE_FONT, { whiteSpace: "pre-wrap" });
}

function computeSourceHeight(
  prepared: PreparedText | null,
  width: number,
): number {
  if (!prepared) return 0;
  // Reserve at least one line of space even for empty-ish cells
  const contentWidth = Math.max(width - 88, 100); // gutter + ribbon + padding
  const { height } = layout(prepared, contentWidth, CODE_LINE_HEIGHT);
  return height;
}

function estimateOutputHeight(outputs: JupyterOutput[]): number {
  if (outputs.length === 0) return 0;

  let total = 0;
  for (const output of outputs) {
    if (output.output_type === "stream") {
      // Use pretext for stream text
      const text = output.text;
      if (text) {
        const p = prepare(text, CODE_FONT, { whiteSpace: "pre-wrap" });
        const contentWidth = Math.max(_containerWidth - 88, 100);
        total += layout(p, contentWidth, CODE_LINE_HEIGHT).height;
      } else {
        total += CODE_LINE_HEIGHT;
      }
    } else if (output.output_type === "error") {
      // Estimate from traceback lines
      const lineCount = output.traceback?.length ?? 1;
      total += lineCount * CODE_LINE_HEIGHT * 2; // traceback lines tend to be multi-line
    } else {
      // display_data / execute_result — inspect MIME types
      const data = output.data;
      if (data) {
        // Use the highest-priority MIME type for estimation
        if ("text/plain" in data && Object.keys(data).length === 1) {
          const text = data["text/plain"] as string;
          const p = prepare(text, CODE_FONT, { whiteSpace: "pre-wrap" });
          const contentWidth = Math.max(_containerWidth - 88, 100);
          total += layout(p, contentWidth, CODE_LINE_HEIGHT).height;
        } else {
          // Find the best estimate from the MIME type
          let estimated = false;
          for (const mime of Object.keys(data)) {
            if (mime in OUTPUT_HEIGHT_DEFAULTS) {
              total += OUTPUT_HEIGHT_DEFAULTS[mime];
              estimated = true;
              break;
            }
          }
          if (!estimated) {
            total += DEFAULT_OUTPUT_ESTIMATE;
          }
        }
      } else {
        total += DEFAULT_OUTPUT_ESTIMATE;
      }
    }
  }
  return total;
}

function buildEntry(cell: NotebookCell): CellHeightEntry {
  const prepared = cell.source ? prepareSource(cell.source) : null;
  const sourceHeight = computeSourceHeight(prepared, _containerWidth);

  const outputs = cell.cell_type === "code" ? cell.outputs : [];
  const outputHeight = estimateOutputHeight(outputs);

  const chromeHeight =
    cell.cell_type === "code" ? DEFAULT_CHROME_HEIGHT : MARKDOWN_CHROME_HEIGHT;

  return {
    sourceHeight,
    outputHeight,
    chromeHeight,
    totalHeight: sourceHeight + outputHeight + chromeHeight,
    prepared,
    preparedSource: cell.source,
    outputMeasured: false,
  };
}

// ── Public mutation API ────────────────────────────────────────────────

/** Initialize or update the cache for a batch of cells. */
export function updateHeightCache(cells: Map<string, NotebookCell>): void {
  let changed = false;

  for (const [id, cell] of cells) {
    const existing = _entries.get(id);

    // Skip if source unchanged and outputs haven't changed
    if (existing && existing.preparedSource === cell.source) {
      // Check if outputs changed (code cells only)
      if (cell.cell_type === "code" && !existing.outputMeasured) {
        const newOutputHeight = estimateOutputHeight(cell.outputs);
        if (newOutputHeight !== existing.outputHeight) {
          existing.outputHeight = newOutputHeight;
          existing.totalHeight =
            existing.sourceHeight + newOutputHeight + existing.chromeHeight;
          changed = true;
        }
      }
      continue;
    }

    _entries.set(id, buildEntry(cell));
    changed = true;
  }

  // Remove entries for cells that no longer exist
  for (const id of _entries.keys()) {
    if (!cells.has(id)) {
      _entries.delete(id);
      changed = true;
    }
  }

  if (changed) emit();
}

/**
 * Called by ResizeObserver when a cell's actual DOM height is measured.
 * Replaces the estimated output height with the real one.
 */
export function setMeasuredHeight(
  cellId: string,
  measuredOutputHeight: number,
): void {
  const entry = _entries.get(cellId);
  if (!entry) return;

  // Only update if the measurement differs meaningfully (>1px)
  if (
    entry.outputMeasured &&
    Math.abs(entry.outputHeight - measuredOutputHeight) < 1
  ) {
    return;
  }

  entry.outputHeight = measuredOutputHeight;
  entry.outputMeasured = true;
  entry.totalHeight =
    entry.sourceHeight + entry.outputHeight + entry.chromeHeight;
  emit();
}

/**
 * Called by ResizeObserver for the full cell container height.
 * Derives chrome height from the first measurement.
 */
export function setMeasuredCellHeight(
  cellId: string,
  totalMeasured: number,
): void {
  const entry = _entries.get(cellId);
  if (!entry) return;

  // If we have both source and output measurements, derive chrome
  if (entry.outputMeasured) {
    const derivedChrome =
      totalMeasured - entry.sourceHeight - entry.outputHeight;
    if (derivedChrome > 0 && Math.abs(derivedChrome - entry.chromeHeight) > 2) {
      entry.chromeHeight = derivedChrome;
    }
  }

  // Always update total from measurement when available
  if (Math.abs(entry.totalHeight - totalMeasured) > 1) {
    entry.totalHeight = totalMeasured;
    emit();
  }
}

/** Mark a cell's output as unmeasured (e.g., after re-execution). */
export function invalidateOutputHeight(cellId: string): void {
  const entry = _entries.get(cellId);
  if (entry) {
    entry.outputMeasured = false;
  }
}

/**
 * Update container width and re-layout all cells.
 * This is the hot resize path — pure arithmetic, ~0.0002ms per cell.
 */
export function setContainerWidth(width: number): void {
  if (Math.abs(width - _containerWidth) < 1) return;
  _containerWidth = width;

  let changed = false;
  for (const entry of _entries.values()) {
    if (entry.prepared) {
      const newSourceHeight = computeSourceHeight(entry.prepared, width);
      if (newSourceHeight !== entry.sourceHeight) {
        entry.sourceHeight = newSourceHeight;
        // Re-estimate outputs that use pretext (text/plain, stream)
        // For measured outputs, keep the measured value
        if (!entry.outputMeasured) {
          entry.totalHeight =
            newSourceHeight + entry.outputHeight + entry.chromeHeight;
        } else {
          entry.totalHeight =
            newSourceHeight + entry.outputHeight + entry.chromeHeight;
        }
        changed = true;
      }
    }
  }

  if (changed) emit();
}

/** Get the current container width. */
export function getContainerWidth(): number {
  return _containerWidth;
}
