/**
 * React wrapper for the Sift table engine.
 *
 * Usage:
 *   <SiftTable data={tableData} onChange={handleState} />
 *
 * Or with a URL (auto-detects Arrow IPC vs Parquet):
 *   <SiftTable url="/data.arrow" onChange={handleState} />
 *   <SiftTable url="/data.parquet" onChange={handleState} />
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
import { ensureModule, getModuleSync } from "./predicate";
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
  /** Pre-built TableData object. Mutually exclusive with `url`. */
  data?: TableData;
  /** URL to load data from (Arrow IPC or Parquet, auto-detected). Mutually exclusive with `data`. */
  url?: string;
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

// --- Format detection ---

/** Parquet magic bytes: PAR1 */
const PARQUET_MAGIC = new Uint8Array([0x50, 0x41, 0x52, 0x31]);

/**
 * Detect whether a fetch response contains Parquet or Arrow IPC data.
 * Checks Content-Type header first, then falls back to magic byte inspection.
 * Returns the format and the response bytes (buffered for parquet, or a
 * reconstructed ReadableStream for Arrow IPC to preserve streaming).
 */
async function detectFormat(
  response: Response,
): Promise<
  | { format: "parquet"; bytes: Uint8Array }
  | { format: "arrow-ipc"; stream: ReadableStream<Uint8Array> }
> {
  const contentType = response.headers.get("Content-Type") ?? "";
  if (contentType.includes("parquet")) {
    return { format: "parquet", bytes: new Uint8Array(await response.arrayBuffer()) };
  }
  if (contentType.includes("arrow") || contentType.includes("ipc")) {
    if (!response.body) throw new Error("Response has no body");
    return { format: "arrow-ipc", stream: response.body };
  }

  // Ambiguous content type — peek magic bytes
  if (!response.body) throw new Error("Response has no body");
  const reader = response.body.getReader();
  const { value: firstChunk, done } = await reader.read();

  if (done || !firstChunk || firstChunk.length < 4) {
    // Too small to detect — try Arrow IPC as default
    const empty = firstChunk ?? new Uint8Array(0);
    return {
      format: "arrow-ipc",
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue(empty);
          controller.close();
        },
      }),
    };
  }

  const isParquet =
    firstChunk[0] === PARQUET_MAGIC[0] &&
    firstChunk[1] === PARQUET_MAGIC[1] &&
    firstChunk[2] === PARQUET_MAGIC[2] &&
    firstChunk[3] === PARQUET_MAGIC[3];

  if (isParquet) {
    // Buffer the rest for parquet (needs random access)
    const chunks: Uint8Array[] = [firstChunk];
    while (true) {
      const { value, done: streamDone } = await reader.read();
      if (streamDone) break;
      chunks.push(value);
    }
    const totalLength = chunks.reduce((sum, c) => sum + c.length, 0);
    const bytes = new Uint8Array(totalLength);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    return { format: "parquet", bytes };
  }

  // Reconstruct stream with peeked chunk for Arrow IPC streaming
  return {
    format: "arrow-ipc",
    stream: new ReadableStream({
      start(controller) {
        controller.enqueue(firstChunk);
      },
      async pull(controller) {
        const { value, done: streamDone } = await reader.read();
        if (streamDone) {
          controller.close();
        } else {
          controller.enqueue(value);
        }
      },
      cancel() {
        reader.cancel();
      },
    }),
  };
}

// --- WASM summary computation ---

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
        return {
          kind: "timestamp" as const,
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
        };
      }
      case "numeric": {
        const bins = mod.store_histogram(handle, c, BIN_COUNT) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        return {
          kind: "numeric" as const,
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
        };
      }
    }
  });
}

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

// --- Component ---

export function SiftTable({
  data,
  url,
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

  // Load from URL when `url` prop is provided.
  // Detects format via Content-Type header + magic byte fallback:
  // - Parquet: buffer fully, load via WASM with progressive row groups
  // - Arrow IPC: stream batches (existing behavior)
  useEffect(() => {
    if (!url || !containerRef.current) return;

    let cancelled = false;
    const container = containerRef.current;
    let wasmHandle: number | null = null;
    let engineDiv: HTMLDivElement | null = null;

    function mountEngine(tableData: TableData) {
      if (engineRef.current) {
        engineRef.current.destroy();
        engineRef.current = null;
      }
      engineDiv?.remove();
      engineDiv = document.createElement("div");
      engineDiv.style.height = "100%";
      container.appendChild(engineDiv);
      engineRef.current = createTable(engineDiv, tableData, {
        onChange: stableOnChange,
      });
    }

    async function loadParquet(parquetBytes: Uint8Array) {
      await ensureModule();
      const mod = getModuleSync();
      if (cancelled) return;

      const meta = mod.parquet_metadata(parquetBytes);
      const numRowGroups = meta[0];

      if (numRowGroups === 0) {
        setError("Parquet file has no row groups.");
        setStatus("error");
        return;
      }

      // Load first row group → mount table immediately
      const handle = mod.load_parquet_row_group(parquetBytes, 0, 0);
      wasmHandle = handle;

      const { tableData, columns, prefetchViewport } = createWasmTableData(handle, columnOverrides);
      tableData.prefetchViewport = prefetchViewport;
      tableData.recomputeSummaries = () => updateWasmSummaries(mod, handle, tableData, columns);
      updateWasmSummaries(mod, handle, tableData, columns);

      if (cancelled) return;
      mountEngine(tableData);
      setStatus("ready");

      // Stream remaining row groups progressively
      for (let g = 1; g < numRowGroups; g++) {
        if (cancelled) return;
        await new Promise((r) => setTimeout(r, 0));
        if (cancelled) return;
        mod.load_parquet_row_group(parquetBytes, g, handle);
        tableData.rowCount = mod.num_rows(handle);
        updateWasmSummaries(mod, handle, tableData, columns);
        engineRef.current?.onBatchAppended();
      }

      engineRef.current?.setStreamingDone();
    }

    async function loadArrowIpc(source: Response | ReadableStream<Uint8Array>) {
      const reader = await RecordBatchReader.from(source);
      await reader.open();
      if (cancelled) return;

      const { columns, fieldNames, stringCols, rawCols, accumulators, tableData } = buildTableState(
        reader.schema,
        typeOverrides,
        columnOverrides,
      );

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
        setError("No data in file.");
        setStatus("error");
        return;
      }
      appendBatch(firstResult.value);

      if (cancelled) return;
      mountEngine(tableData);
      setStatus("ready");

      for await (const batch of reader) {
        if (cancelled) break;
        appendBatch(batch);
        engineRef.current?.onBatchAppended();
      }
      engineRef.current?.setStreamingDone();
    }

    async function loadFromUrl() {
      setStatus("loading");
      setError(null);

      const response = await fetch(url!);
      if (!response.ok) {
        throw new Error(`Failed to fetch: ${response.status} ${response.statusText}`);
      }

      const detected = await detectFormat(response);
      if (cancelled) return;

      if (detected.format === "parquet") {
        await loadParquet(detected.bytes);
      } else {
        await loadArrowIpc(detected.stream);
      }
    }

    loadFromUrl().catch((err) => {
      if (!cancelled) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        setStatus("error");
      }
    });

    return () => {
      cancelled = true;
      if (wasmHandle !== null) {
        try {
          getModuleSync().free(wasmHandle);
        } catch {
          /* module may not be loaded */
        }
      }
      engineRef.current?.destroy();
      engineRef.current = null;
      engineDiv?.remove();
    };
  }, [url, typeOverrides, columnOverrides, stableOnChange]);

  return (
    <div ref={containerRef} className={className} style={{ height: "100%", ...style }}>
      {status === "error" && error && <div className="sift-loading">Error: {error}</div>}
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
