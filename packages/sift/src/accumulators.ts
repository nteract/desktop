import type { Field } from "apache-arrow";
import { Type } from "apache-arrow";
import type {
  BooleanColumnSummary,
  CategoricalColumnSummary,
  ColumnSummary,
  ColumnType,
  NumericColumnSummary,
  TimestampColumnSummary,
} from "./table";

// --- Constants ---

export const BIN_COUNT = 25;
export const TOP_CATEGORIES = 3;

// --- Column type detection from Arrow schema ---

export function detectColumnType(field: Field): ColumnType {
  const t = field.type.typeId;
  if (t === Type.Bool) return "boolean";
  if (t === Type.Timestamp || t === Type.Date || t === Type.DateMillisecond || t === Type.DateDay)
    return "timestamp";
  if (
    t === Type.Int ||
    t === Type.Float ||
    t === Type.Decimal ||
    t === Type.Int8 ||
    t === Type.Int16 ||
    t === Type.Int32 ||
    t === Type.Int64 ||
    t === Type.Float16 ||
    t === Type.Float32 ||
    t === Type.Float64
  )
    return "numeric";
  return "categorical";
}

// --- Data-driven type refinement ---

// Values commonly used as null sentinels in CSV-sourced datasets
const NULL_SENTINELS = new Set([
  "?",
  "N/A",
  "NA",
  "n/a",
  "na",
  "NULL",
  "null",
  "None",
  "none",
  "-",
  "",
]);

// ISO-like date patterns: YYYY-MM-DD, YYYY/MM/DD, MM/DD/YYYY, DD-Mon-YYYY, etc.
const DATE_PATTERN = /^\d{4}[-/]\d{1,2}[-/]\d{1,2}$|^\d{1,2}[-/]\d{1,2}[-/]\d{4}$/;

/**
 * Check if a string value is a common null sentinel.
 */
export function isNullSentinel(val: string): boolean {
  return NULL_SENTINELS.has(val);
}

/**
 * Refine a column type by sampling actual data values from the first batch.
 * Only refines 'categorical' columns — other types are already well-detected
 * from the Arrow schema.
 *
 * Returns the refined type and whether null sentinels were detected.
 */
export function refineColumnType(
  schemaType: ColumnType,
  values: unknown[],
  sampleSize = 100,
): { type: ColumnType; hasNullSentinels: boolean } {
  if (schemaType !== "categorical") {
    return { type: schemaType, hasNullSentinels: false };
  }

  const sample = values.slice(0, sampleSize);
  let dateCount = 0;
  let nullSentinelCount = 0;
  let nonNullCount = 0;

  for (const val of sample) {
    if (val == null) continue;
    const s = stringifyValue(val);
    if (NULL_SENTINELS.has(s)) {
      nullSentinelCount++;
      continue;
    }
    nonNullCount++;
    if (DATE_PATTERN.test(s)) {
      dateCount++;
    }
  }

  const hasNullSentinels = nullSentinelCount > 0;

  // If >80% of non-null, non-sentinel values look like dates, treat as timestamp
  if (nonNullCount > 0 && dateCount / nonNullCount > 0.8) {
    return { type: "timestamp", hasNullSentinels };
  }

  return { type: "categorical", hasNullSentinels };
}

// --- Cell formatting ---

export function stringifyValue(val: unknown): string {
  if (Array.isArray(val) || (typeof val === "object" && val !== null)) {
    return JSON.stringify(val, (_k, v) => (typeof v === "bigint" ? Number(v) : v));
  }
  return String(val);
}

export function formatCell(columnType: ColumnType, val: unknown): string {
  if (val == null) return "";
  switch (columnType) {
    case "timestamp": {
      const d = new Date(Number(val));
      return d.toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
      });
    }
    case "boolean":
      return val ? "Yes" : "No";
    default:
      return stringifyValue(val);
  }
}

// --- Summary accumulators ---

export interface SummaryAccumulator {
  add(rawCol: unknown[], startRow: number, count: number): void;
  snapshot(totalRows: number): ColumnSummary;
}

/** Cap for tracking unique numeric values (avoids unbounded memory). */
const UNIQUE_TRACK_LIMIT = 20;

export class NumericAccumulator implements SummaryAccumulator {
  min = Infinity;
  max = -Infinity;
  monotonic = true;
  lastValue = -Infinity;
  private finiteValues: number[] = [];
  private nanCount = 0;
  private infCount = 0;
  private negInfCount = 0;
  private nullCount = 0;
  /** Tracks distinct finite values. Set to null when the cap is exceeded. */
  private uniqueValues: Set<number> | null = new Set();

  add(rawCol: unknown[], startRow: number, count: number) {
    for (let r = startRow; r < startRow + count; r++) {
      const raw = rawCol[r];
      if (raw == null) {
        this.nullCount++;
        continue;
      }
      const v = typeof raw === "bigint" ? Number(raw) : (raw as number);
      if (Number.isNaN(v)) {
        this.nanCount++;
        continue;
      }
      if (v === Infinity) {
        this.infCount++;
        continue;
      }
      if (v === -Infinity) {
        this.negInfCount++;
        continue;
      }
      this.finiteValues.push(v);
      if (this.uniqueValues !== null) {
        this.uniqueValues.add(v);
        if (this.uniqueValues.size > UNIQUE_TRACK_LIMIT) this.uniqueValues = null;
      }
      if (v < this.min) this.min = v;
      if (v > this.max) this.max = v;
      if (v < this.lastValue) this.monotonic = false;
      this.lastValue = v;
    }
  }

  snapshot(totalRows?: number): ColumnSummary {
    if (this.finiteValues.length === 0) return null;

    // Detect index/ID columns: monotonic with range > 0 and high uniqueness, or just high uniqueness
    const uniqueCount = this.uniqueValues?.size;
    const isMonotonicClean =
      this.monotonic &&
      this.min < this.max &&
      this.nanCount === 0 &&
      this.infCount === 0 &&
      this.negInfCount === 0 &&
      this.nullCount === 0;
    const isHighUniqueness =
      uniqueCount !== undefined && totalRows !== undefined && uniqueCount >= totalRows * 0.9;
    const isIndex =
      (isMonotonicClean && isHighUniqueness) || (isMonotonicClean && uniqueCount === undefined);

    const binWidth = (this.max - this.min) / BIN_COUNT || 1;
    const bins: NumericColumnSummary["bins"] = [];
    for (let b = 0; b < BIN_COUNT; b++) {
      bins.push({
        x0: this.min + b * binWidth,
        x1: this.min + (b + 1) * binWidth,
        count: 0,
      });
    }
    for (const v of this.finiteValues) {
      let idx = Math.floor((v - this.min) / binWidth);
      if (idx >= BIN_COUNT) idx = BIN_COUNT - 1;
      if (idx < 0) idx = 0;
      bins[idx].count++;
    }
    return {
      kind: "numeric",
      min: this.min,
      max: this.max,
      bins,
      uniqueCount,
      isIndex,
      nullCount: this.nullCount + this.nanCount,
    };
  }
}

export class TimestampAccumulator implements SummaryAccumulator {
  min = Infinity;
  max = -Infinity;
  private allValues: number[] = [];
  private nullCount = 0;

  add(rawCol: unknown[], startRow: number, count: number) {
    for (let r = startRow; r < startRow + count; r++) {
      const raw = rawCol[r];
      if (raw == null) {
        this.nullCount++;
        continue;
      }
      const v = typeof raw === "bigint" ? Number(raw) : Number(raw);
      if (!Number.isFinite(v)) {
        this.nullCount++;
        continue;
      }
      this.allValues.push(v);
      if (v < this.min) this.min = v;
      if (v > this.max) this.max = v;
    }
  }

  snapshot(): ColumnSummary {
    if (this.allValues.length === 0) return null;
    const binWidth = (this.max - this.min) / BIN_COUNT || 1;
    const bins: TimestampColumnSummary["bins"] = [];
    for (let b = 0; b < BIN_COUNT; b++) {
      bins.push({
        x0: this.min + b * binWidth,
        x1: this.min + (b + 1) * binWidth,
        count: 0,
      });
    }
    for (const v of this.allValues) {
      let idx = Math.floor((v - this.min) / binWidth);
      if (idx >= BIN_COUNT) idx = BIN_COUNT - 1;
      if (idx < 0) idx = 0;
      bins[idx].count++;
    }
    return {
      kind: "timestamp",
      min: this.min,
      max: this.max,
      bins,
      nullCount: this.nullCount,
    };
  }
}

export class CategoricalAccumulator implements SummaryAccumulator {
  private freq = new Map<string, number>();
  private stringCol: string[];

  constructor(stringCol: string[]) {
    this.stringCol = stringCol;
  }

  add(_rawCol: unknown[], startRow: number, count: number) {
    for (let r = startRow; r < startRow + count; r++) {
      const s = this.stringCol[r];
      this.freq.set(s, (this.freq.get(s) ?? 0) + 1);
    }
  }

  snapshot(totalRows: number): ColumnSummary {
    const sorted = [...this.freq.entries()].sort((a, b) => b[1] - a[1]);
    const allCategories = sorted.map(([label, count]) => ({
      label,
      count,
      pct: Math.round((count / totalRows) * 1000) / 10,
    }));
    const topCategories = allCategories.slice(0, TOP_CATEGORIES);
    const othersCount = sorted.slice(TOP_CATEGORIES).reduce((s, e) => s + e[1], 0);
    const othersPct = Math.round((othersCount / totalRows) * 1000) / 10;

    // Compute median text length across unique values
    const lengths = sorted.map(([label]) => label.length).sort((a, b) => a - b);
    const medianTextLength = lengths.length > 0 ? lengths[Math.floor(lengths.length / 2)] : 0;

    return {
      kind: "categorical",
      uniqueCount: this.freq.size,
      topCategories,
      othersCount,
      othersPct,
      allCategories,
      medianTextLength,
    } satisfies CategoricalColumnSummary;
  }
}

export class BooleanAccumulator implements SummaryAccumulator {
  trueCount = 0;
  falseCount = 0;
  nullCount = 0;

  add(rawCol: unknown[], startRow: number, count: number) {
    for (let r = startRow; r < startRow + count; r++) {
      const v = rawCol[r];
      if (v == null) this.nullCount++;
      else if (v) this.trueCount++;
      else this.falseCount++;
    }
  }

  snapshot(totalRows: number): ColumnSummary {
    return {
      kind: "boolean",
      trueCount: this.trueCount,
      falseCount: this.falseCount,
      nullCount: this.nullCount,
      total: totalRows,
    } satisfies BooleanColumnSummary;
  }
}
