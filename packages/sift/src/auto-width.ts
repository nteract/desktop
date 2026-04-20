/**
 * Column width utilities.
 *
 * autoWidth() — initial width from header label measurement + type minimums.
 * fitColumnWidths() — refine widths by sampling actual cell data via pretext.
 */
import { measureNaturalWidth, prepareWithSegments } from "@chenglou/pretext";
import type { ColumnType, TableData } from "./table";

const LABEL_FONT = '600 11px Inter, "Helvetica Neue", Helvetica, Arial, sans-serif';
const CELL_FONT = '14px Inter, "Helvetica Neue", Helvetica, Arial, sans-serif';
const HEADER_CHROME = 60; // cell padding + type icon + sort arrow
const CELL_PAD = 24; // 12px each side — matches CELL_PAD_H in table.ts

/** Widest forced line for `text` at `font`. Same engine as the table's layout. */
function measureText(text: string, font: string): number {
  return measureNaturalWidth(prepareWithSegments(text, font));
}

/** Compute initial column width from header label + type constraints */
export function autoWidth(name: string, colType: ColumnType): number {
  const labelW = measureText(name.toUpperCase(), LABEL_FONT) + HEADER_CHROME;

  switch (colType) {
    case "boolean":
      return Math.max(90, Math.ceil(labelW));
    case "timestamp":
      return Math.max(130, Math.ceil(labelW));
    case "numeric":
      return Math.max(100, Math.ceil(labelW));
    case "categorical":
      return Math.max(120, Math.min(280, Math.ceil(labelW)));
  }
}

/**
 * Refine column widths by sampling actual cell data.
 * Uses pretext prepareWithSegments for accurate text measurement —
 * this matches the table engine's own layout calculations.
 *
 * Uses the median single-line width — avoids outlier-driven expansion.
 * Only widens columns, never shrinks below the header-based width.
 * Narrows index columns to save space.
 */
export function fitColumnWidths(data: TableData, colWidths: number[], maxWidth = 300): void {
  const sampleSize = Math.min(30, data.rowCount);
  if (sampleSize === 0) return;

  for (let c = 0; c < data.columns.length; c++) {
    // Size index columns to fit their max value — they just show numbers
    const summary = data.columnSummaries[c];
    if (summary && (summary as any).isIndex === true) {
      const maxVal = (summary as any).max as number;
      const formatted = maxVal != null ? Math.round(maxVal).toLocaleString() : "";
      const indexW = Math.ceil(measureText(formatted, CELL_FONT)) + CELL_PAD;
      // Only widen — main.ts may have set a larger width from totalRows
      if (indexW > colWidths[c]) colWidths[c] = indexW;
      continue;
    }

    const widths: number[] = [];
    for (let r = 0; r < sampleSize; r++) {
      const text = data.getCell(r, c);
      if (!text) continue;
      const w = measureText(text, CELL_FONT) + CELL_PAD;
      widths.push(w);
    }
    if (widths.length === 0) continue;

    // Use median — stable, not skewed by long outliers
    widths.sort((a, b) => a - b);
    const median = widths[Math.floor(widths.length / 2)];
    const fitted = Math.min(maxWidth, Math.ceil(median));

    // Only widen, never shrink below header-based width
    if (fitted > colWidths[c]) {
      colWidths[c] = fitted;
    }
  }
}
