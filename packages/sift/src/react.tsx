/**
 * React wrapper for the Sift table engine.
 *
 * Usage:
 *   <SiftTable data={tableData} onChange={handleState} />
 *
 * Or with Arrow IPC URL:
 *   <SiftTable url="/data.arrow" onChange={handleState} />
 *
 * The component manages the imperative TableEngine lifecycle —
 * mounting on first render, updating on data changes, and
 * cleaning up on unmount.
 */

import type { RecordBatch, Schema } from "apache-arrow";
import { RecordBatchReader } from "apache-arrow";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  BooleanAccumulator,
  CategoricalAccumulator,
  detectColumnType,
  formatCell,
  NumericAccumulator,
  type SummaryAccumulator,
  TimestampAccumulator,
} from "./accumulators";
import { getModuleSync, isAvailable } from "./predicate";
import {
  type Column,
  type ColumnFilter,
  type ColumnType,
  createTable,
  type TableData,
  type TableEngine,
  type TableEngineState,
} from "./table";
import { createWasmTableData } from "./wasm-table-data";

// --- Props ---

export type SiftTableProps = {
  /** Pre-built TableData object. Mutually exclusive with `url` and `parquetUrl`. */
  data?: TableData;
  /** Arrow IPC URL to stream from. Mutually exclusive with `data` and `parquetUrl`. */
  url?: string;
  /** Parquet URL to load via WASM. Mutually exclusive with `data` and `url`. */
  parquetUrl?: string;
  /** Column type overrides keyed by column name. */
  typeOverrides?: Record<string, ColumnType>;
  /** Column display overrides (label, width, sortable). */
  columnOverrides?: Record<string, Partial<Column>>;
  /** Called whenever sort or filter state changes from UI interaction. */
  onChange?: (state: TableEngineState) => void;
  /** CSS class name for the container div. */
  className?: string;
  /** Inline styles for the container div. */
  style?: React.CSSProperties;
};

import { autoWidth } from "./auto-width";

// --- Helpers ---

function buildTableState(
  schema: Schema,
  typeOverrides: Record<string, ColumnType> = {},
  columnOverrides: Record<string, Partial<Column>> = {},
) {
  const fieldNames = schema.fields.map((f) => f.name);

  const columns: Column[] = schema.fields.map((field) => {
    const colType = typeOverrides[field.name] ?? detectColumnType(field);
    const overrides = columnOverrides[field.name];
    return {
      key: field.name,
      label: overrides?.label ?? field.name,
      width: overrides?.width ?? autoWidth(field.name, colType),
      sortable: overrides?.sortable ?? true,
      numeric: colType === "numeric",
      columnType: colType,
    };
  });

  const stringCols: string[][] = fieldNames.map(() => []);
  const rawCols: unknown[][] = fieldNames.map(() => []);

  const accumulators: SummaryAccumulator[] = columns.map((col, c) => {
    switch (col.columnType) {
      case "numeric":
        return new NumericAccumulator();
      case "timestamp":
        return new TimestampAccumulator();
      case "boolean":
        return new BooleanAccumulator();
      case "categorical":
        return new CategoricalAccumulator(stringCols[c]);
    }
  });

  const tableData: TableData = {
    columns,
    rowCount: 0,
    getCell: (row, col) => stringCols[col][row],
    getCellRaw: (row, col) => rawCols[col][row],
    columnSummaries: columns.map(() => null),
  };

  return { columns, fieldNames, stringCols, rawCols, accumulators, tableData };
}

// --- WASM summary helper ---

/** Compute unfiltered summaries from the WASM store and update tableData. */
function updateWasmSummaries(
  mod: ReturnType<typeof getModuleSync>,
  handle: number,
  tableData: TableData,
  columns: Column[],
) {
  const numRows = mod.num_rows(handle);
  const BIN_COUNT = 25;

  tableData.rowCount = numRows;
  tableData.columnSummaries = columns.map((col, c) => {
    switch (col.columnType) {
      case "categorical": {
        const counts = mod.store_value_counts(handle, c) as {
          label: string;
          count: number;
        }[];
        const allCategories = counts.map(({ label, count }) => ({
          label,
          count,
          pct: Math.round((count / numRows) * 1000) / 10,
        }));
        const topCategories = allCategories.slice(0, 3);
        const othersCount = counts.slice(3).reduce((s, e) => s + e.count, 0);
        const othersPct = Math.round((othersCount / numRows) * 1000) / 10;
        const lengths = counts.map(({ label }) => label.length).sort((a, b) => a - b);
        const medianTextLength = lengths.length > 0 ? lengths[Math.floor(lengths.length / 2)] : 0;

        return {
          kind: "categorical" as const,
          uniqueCount: counts.length,
          topCategories,
          othersCount,
          othersPct,
          allCategories,
          medianTextLength,
        };
      }
      case "boolean": {
        const [trueCount, falseCount, nullCount] = mod.store_bool_counts(handle, c);
        return {
          kind: "boolean" as const,
          trueCount,
          falseCount,
          nullCount,
          total: numRows,
        };
      }
      case "timestamp": {
        const bins = mod.store_temporal_histogram(handle, c) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        const first = bins[0];
        const last = bins[bins.length - 1];
        const totalInBins = bins.reduce((s, b) => s + b.count, 0);
        const nullCount = numRows - totalInBins;
        return {
          kind: "timestamp" as const,
          min: first.x0,
          max: last.x1,
          bins,
          nullCount: nullCount > 0 ? nullCount : undefined,
        };
      }
      case "numeric": {
        const bins = mod.store_histogram(handle, c, BIN_COUNT) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        const nFirst = bins[0];
        const nLast = bins[bins.length - 1];
        const totalInBins = bins.reduce((s, b) => s + b.count, 0);
        const nullCount = numRows - totalInBins;
        return {
          kind: "numeric" as const,
          min: nFirst.x0,
          max: nLast.x1,
          bins,
          nullCount: nullCount > 0 ? nullCount : undefined,
        };
      }
      default:
        return null;
    }
  });
}

// --- Component ---

export function SiftTable({
  data,
  url,
  parquetUrl,
  typeOverrides,
  columnOverrides,
  onChange,
  className,
  style,
}: SiftTableProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const engineRef = useRef<TableEngine | null>(null);
  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [error, setError] = useState<string | null>(null);

  // Stable callback ref to avoid re-mounting engine when onChange identity changes
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const stableOnChange = useCallback((state: TableEngineState) => {
    onChangeRef.current?.(state);
  }, []);

  // Mount engine when `data` prop is provided directly
  useEffect(() => {
    if (!data || !containerRef.current) return;

    // Clean up previous engine
    if (engineRef.current) {
      engineRef.current.destroy();
      engineRef.current = null;
    }

    // Create a dedicated div for the engine so it doesn't conflict with React's DOM
    const engineDiv = document.createElement("div");
    engineDiv.style.height = "100%";
    containerRef.current.appendChild(engineDiv);

    engineRef.current = createTable(engineDiv, data, {
      onChange: stableOnChange,
    });
    setStatus("ready");

    return () => {
      engineRef.current?.destroy();
      engineRef.current = null;
      engineDiv.remove();
    };
  }, [data, stableOnChange]);

  // Stream from URL when `url` prop is provided
  useEffect(() => {
    if (!url || !containerRef.current) return;

    let cancelled = false;
    const container = containerRef.current;

    async function loadFromUrl() {
      setStatus("loading");
      setError(null);

      try {
        const response = await fetch(url!);
        if (!response.ok) {
          throw new Error(`Failed to fetch: ${response.status} ${response.statusText}`);
        }

        const reader = await RecordBatchReader.from(response);
        await reader.open();

        if (cancelled) return;

        const { columns, fieldNames, stringCols, rawCols, accumulators, tableData } =
          buildTableState(reader.schema, typeOverrides, columnOverrides);

        let totalRows = 0;

        function appendBatch(batch: RecordBatch) {
          const batchRows = batch.numRows;
          const startRow = totalRows;
          for (let c = 0; c < fieldNames.length; c++) {
            const col = batch.getChild(fieldNames[c])!;
            for (let r = 0; r < batchRows; r++) {
              const val = col.get(r);
              rawCols[c].push(val);
              stringCols[c].push(formatCell(columns[c].columnType, val));
            }
            accumulators[c].add(rawCols[c], startRow, batchRows);
          }
          totalRows += batchRows;
          tableData.rowCount = totalRows;
          tableData.columnSummaries = accumulators.map((a) => a.snapshot(totalRows));
        }

        const firstResult = await reader.next();
        if (cancelled) return;
        if (firstResult.done) {
          setError("No data in Arrow file.");
          setStatus("error");
          return;
        }
        appendBatch(firstResult.value);

        // Clean up previous engine before creating new one
        if (engineRef.current) {
          engineRef.current.destroy();
          engineRef.current = null;
        }

        // Create a dedicated div for the engine — don't touch React-managed children
        const engineDiv = document.createElement("div");
        engineDiv.style.height = "100%";
        container.appendChild(engineDiv);

        engineRef.current = createTable(engineDiv, tableData, {
          onChange: stableOnChange,
        });
        setStatus("ready");

        // Stream remaining batches
        for await (const batch of reader) {
          if (cancelled) break;
          appendBatch(batch);
          engineRef.current!.onBatchAppended();
        }
        engineRef.current!.setStreamingDone();
      } catch (err) {
        if (cancelled) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        setStatus("error");
      }
    }

    loadFromUrl();

    return () => {
      cancelled = true;
      engineRef.current?.destroy();
      engineRef.current = null;
    };
  }, [url, typeOverrides, columnOverrides, stableOnChange]);

  // Load from parquet URL via WASM when `parquetUrl` prop is provided
  useEffect(() => {
    if (!parquetUrl || !containerRef.current) return;

    let cancelled = false;
    let mountDiv: HTMLDivElement | null = null;
    let wasmHandle: number | undefined;
    const container = containerRef.current;

    // typeOverrides is intentionally not used here — WASM detects types from the Arrow schema
    async function loadFromParquetUrl() {
      setStatus("loading");
      setError(null);

      try {
        // Start WASM init in parallel with data fetch
        const [response, wasmOk] = await Promise.all([fetch(parquetUrl!), isAvailable()]);

        if (cancelled) return;

        if (!wasmOk) {
          throw new Error("Failed to load nteract-predicate WASM module");
        }
        if (!response.ok) {
          throw new Error(`Failed to fetch: ${response.status} ${response.statusText}`);
        }

        const parquetBytes = new Uint8Array(await response.arrayBuffer());
        if (cancelled) return;

        const mod = getModuleSync();

        // Get metadata to know how many row groups
        const meta = mod.parquet_metadata(parquetBytes);
        const numRowGroups = meta[0];

        // Load first row group → mount table immediately
        wasmHandle = mod.load_parquet_row_group(parquetBytes, 0, 0);
        const handle = wasmHandle;

        const { tableData, columns, prefetchViewport } = createWasmTableData(
          handle,
          columnOverrides,
        );
        tableData.prefetchViewport = prefetchViewport;
        tableData.recomputeSummaries = () => updateWasmSummaries(mod, handle, tableData, columns);

        // Compute initial summaries from first row group
        updateWasmSummaries(mod, handle, tableData, columns);

        if (cancelled) return;

        // Clean up previous engine before creating new one
        if (engineRef.current) {
          engineRef.current.destroy();
          engineRef.current = null;
        }

        // Create a dedicated div for the engine — don't touch React-managed children
        mountDiv = document.createElement("div");
        mountDiv.style.height = "100%";
        container.appendChild(mountDiv);

        engineRef.current = createTable(mountDiv, tableData, {
          onChange: stableOnChange,
        });
        setStatus("ready");

        // Stream remaining row groups progressively
        for (let g = 1; g < numRowGroups; g++) {
          if (cancelled) break;
          // Yield to the event loop so the UI stays responsive
          await new Promise((r) => setTimeout(r, 0));
          if (cancelled) break;
          mod.load_parquet_row_group(parquetBytes, g, handle);
          tableData.rowCount = mod.num_rows(handle);
          updateWasmSummaries(mod, handle, tableData, columns);
          engineRef.current?.onBatchAppended();
        }

        if (!cancelled) {
          engineRef.current?.setStreamingDone();
        }
      } catch (err) {
        if (cancelled) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        setStatus("error");
      }
    }

    loadFromParquetUrl();

    return () => {
      cancelled = true;
      engineRef.current?.destroy();
      engineRef.current = null;
      mountDiv?.remove();
      if (wasmHandle !== undefined) {
        getModuleSync().free(wasmHandle);
      }
    };
  }, [parquetUrl, columnOverrides, stableOnChange]);

  return (
    <div ref={containerRef} className={className} style={{ height: "100%", ...style }}>
      {status === "error" && error && <div className="pt-loading">Error: {error}</div>}
    </div>
  );
}

// --- Imperative handle for advanced use ---

export type SiftTableHandle = {
  engine: TableEngine | null;
  setFilter: (colIndex: number, filter: ColumnFilter) => void;
  clearAllFilters: () => void;
  getState: () => TableEngineState | null;
};

/**
 * Hook to get an imperative handle to the table engine.
 * Use with a ref: const handleRef = useSiftHandle()
 * Then pass handleRef to SiftTable (not yet wired — future forwardRef).
 */
export function useSiftEngine(engine: TableEngine | null): SiftTableHandle {
  return {
    engine,
    setFilter: (colIndex, filter) => engine?.setFilter(colIndex, filter),
    clearAllFilters: () => engine?.clearAllFilters(),
    getState: () => engine?.getState() ?? null,
  };
}

export type { ExplorerState, FilterPredicate, SortEntry } from "./filter-schema";
export {
  engineStateToExplorerState,
  explorerStateToJSON,
  predicateToEnglish,
  predicateToPandas,
  predicateToSQL,
} from "./filter-schema";
// Re-export key types and utilities for consumer convenience
export type { Column, ColumnFilter, ColumnType, TableData, TableEngine, TableEngineState };
