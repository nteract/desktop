/**
 * Creates a TableData backed by the nteract-predicate WASM data store.
 *
 * Data lives in WASM memory. Cell access uses viewport caching:
 * prefetchViewport() loads visible rows in one WASM call, then
 * getCell/getCellRaw read from the JS-side cache — no per-cell FFI.
 */
import { tableFromIPC } from "apache-arrow";
import { formatCell } from "./accumulators";
import { autoWidth } from "./auto-width";
import type { FilterSpecJson } from "./predicate";
import { getModuleSync } from "./predicate";
import type { Column, ColumnType, TableData } from "./table";

/** Map WASM col_type strings to our ColumnType */
function mapColType(wasmType: string): ColumnType {
  switch (wasmType) {
    case "numeric":
      return "numeric";
    case "boolean":
      return "boolean";
    case "timestamp":
      return "timestamp";
    default:
      return "categorical";
  }
}

export type WasmTableHandle = {
  handle: number;
  tableData: TableData;
  columns: Column[];
  /** Prefetch visible rows into a JS-side cache. Call before render. */
  prefetchViewport: (dataRowIndices: number[]) => void;
};

/**
 * Build a TableData from a WASM store handle.
 * The module must already be initialized (call ensureModule() first).
 */
export function createWasmTableData(
  handle: number,
  columnOverrides?: Record<string, Partial<Column>>,
): WasmTableHandle {
  const mod = getModuleSync();

  const numRows = mod.num_rows(handle);
  const numCols = mod.num_cols(handle);
  const names: string[] = mod.col_names(handle);

  const columns: Column[] = [];
  for (let c = 0; c < numCols; c++) {
    const wasmType = mod.col_type(handle, c);
    const colType = mapColType(wasmType);
    const overrides = columnOverrides?.[names[c]];
    columns.push({
      key: names[c],
      label: overrides?.label ?? names[c],
      width: overrides?.width ?? autoWidth(names[c], colType),
      sortable: overrides?.sortable ?? true,
      numeric: colType === "numeric",
      columnType: colType,
    });
  }

  // Viewport cache: maps data row index → { strings[], raws[] }
  const cache = new Map<number, { strings: string[]; raws: unknown[] }>();

  function prefetchViewport(dataRowIndices: number[]) {
    if (dataRowIndices.length === 0) return;

    // Check if all requested rows are already cached
    const uncached = dataRowIndices.filter((r) => !cache.has(r));
    if (uncached.length === 0) return;

    // Fetch uncached rows in one WASM call
    const indices = new Uint32Array(uncached);
    const ipcBytes = mod.get_viewport_by_indices(handle, indices);
    const table = tableFromIPC(ipcBytes);

    // Populate cache from the Arrow table
    for (let i = 0; i < uncached.length; i++) {
      const dataRow = uncached[i];
      const strings: string[] = [];
      const raws: unknown[] = [];

      for (let c = 0; c < numCols; c++) {
        const col = table.getChildAt(c)!;
        const val = col.get(i);
        const colType = columns[c].columnType;

        if (val == null) {
          strings.push("");
          raws.push(null);
        } else if (colType === "boolean") {
          const boolVal = typeof val === "boolean" ? val : Boolean(val);
          strings.push(boolVal ? "Yes" : "No");
          raws.push(boolVal);
        } else if (colType === "timestamp") {
          const numVal = typeof val === "bigint" ? Number(val) : Number(val);
          strings.push(formatCell("timestamp", numVal));
          raws.push(numVal);
        } else if (colType === "numeric") {
          const numVal = typeof val === "bigint" ? Number(val) : Number(val);
          strings.push(String(val));
          raws.push(numVal);
        } else {
          strings.push(String(val));
          raws.push(val);
        }
      }

      cache.set(dataRow, { strings, raws });
    }
  }

  const tableData: TableData = {
    columns,
    rowCount: numRows,
    getCell(row: number, col: number): string {
      // Try cache first (populated by prefetchViewport)
      const cached = cache.get(row);
      if (cached) return cached.strings[col];

      // Fallback to per-cell WASM call
      if (mod.is_null(handle, row, col)) return "";
      const colType = columns[col].columnType;
      if (colType === "timestamp") {
        const v = mod.get_cell_f64(handle, row, col);
        if (Number.isFinite(v)) return formatCell("timestamp", v);
      }
      return mod.get_cell_string(handle, row, col);
    },
    getCellRaw(row: number, col: number): unknown {
      // Try cache first
      const cached = cache.get(row);
      if (cached) return cached.raws[col];

      // Fallback to per-cell WASM call
      if (mod.is_null(handle, row, col)) return null;
      const colType = columns[col].columnType;
      if (colType === "numeric" || colType === "timestamp") {
        return mod.get_cell_f64(handle, row, col);
      }
      if (colType === "boolean") {
        return mod.get_cell_string(handle, row, col) === "Yes";
      }
      return mod.get_cell_string(handle, row, col);
    },
    columnSummaries: columns.map(() => null),
    castColumn(colIndex: number, targetType: ColumnType) {
      mod.cast_column(handle, colIndex, targetType);
      // Clear viewport cache — cell values have changed
      cache.clear();
      // Update column metadata
      columns[colIndex].columnType = targetType;
      columns[colIndex].numeric = targetType === "numeric";
    },
    undoCastColumn(colIndex: number): ColumnType {
      const originalType = mod.undo_cast_column(handle, colIndex) as ColumnType;
      cache.clear();
      columns[colIndex].columnType = originalType;
      columns[colIndex].numeric = originalType === "numeric";
      return originalType;
    },
    isColumnCast(colIndex: number): boolean {
      return mod.has_original_column(handle, colIndex);
    },
    sortColumn(colIndex: number, ascending: boolean): Uint32Array {
      return mod.store_sort_indices(handle, colIndex, ascending);
    },
    recomputeFilteredSummaries(mask: Uint8Array, filteredRowCount: number) {
      const BIN_COUNT = 25;
      for (let c = 0; c < numCols; c++) {
        const colType = columns[c].columnType;
        switch (colType) {
          case "categorical": {
            const counts = mod.store_filtered_value_counts(handle, c, mask) as {
              label: string;
              count: number;
            }[];
            const allCategories = counts.map(({ label, count }) => ({
              label,
              count,
              pct: Math.round((count / filteredRowCount) * 1000) / 10,
            }));
            const topCategories = allCategories.slice(0, 3);
            const othersCount = counts.slice(3).reduce((s, e) => s + e.count, 0);
            const othersPct = Math.round((othersCount / filteredRowCount) * 1000) / 10;
            const lengths = counts.map(({ label }) => label.length).sort((a, b) => a - b);
            const medianTextLength =
              lengths.length > 0 ? lengths[Math.floor(lengths.length / 2)] : 0;
            tableData.columnSummaries[c] = {
              kind: "categorical" as const,
              uniqueCount: counts.length,
              topCategories,
              othersCount,
              othersPct,
              allCategories,
              medianTextLength,
            };
            break;
          }
          case "boolean": {
            const [trueCount, falseCount, nullCount] = mod.store_filtered_bool_counts(
              handle,
              c,
              mask,
            );
            tableData.columnSummaries[c] = {
              kind: "boolean" as const,
              trueCount,
              falseCount,
              nullCount,
              total: filteredRowCount,
            };
            break;
          }
          case "numeric":
          case "timestamp": {
            const bins = mod.store_filtered_histogram(handle, c, mask, BIN_COUNT) as {
              x0: number;
              x1: number;
              count: number;
            }[];
            if (bins.length === 0) {
              tableData.columnSummaries[c] = null;
            } else {
              tableData.columnSummaries[c] = {
                kind: colType as "numeric" | "timestamp",
                min: bins[0].x0,
                max: bins[bins.length - 1].x1,
                bins,
              };
            }
            break;
          }
        }
      }
    },
    filterRows(filters: (import("./table").ColumnFilter | null)[]): Uint32Array {
      const specs: FilterSpecJson[] = [];
      for (let c = 0; c < filters.length; c++) {
        const f = filters[c];
        if (!f) continue;
        switch (f.kind) {
          case "range":
            specs.push({ kind: "range", col: c, min: f.min, max: f.max });
            break;
          case "set":
            specs.push({ kind: "set", col: c, values: Array.from(f.values) });
            break;
          case "boolean":
            specs.push({ kind: "boolean", col: c, value: f.value });
            break;
        }
      }
      return mod.store_filter_rows(handle, specs);
    },
  };

  return { handle, tableData, columns, prefetchViewport };
}
