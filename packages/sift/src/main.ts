import type { RecordBatch } from "apache-arrow";
import { RecordBatchReader } from "apache-arrow";
import { catchError, defer, EMPTY, Observable, Subject, switchMap } from "rxjs";
import {
  BooleanAccumulator,
  CategoricalAccumulator,
  detectColumnType,
  formatCell,
  NumericAccumulator,
  type SummaryAccumulator,
  TimestampAccumulator,
} from "./accumulators";
import { autoWidth } from "./auto-width";
import { DATASETS, type DatasetEntry } from "./datasets";
import { resolveHuggingFaceParquetUrl } from "./parquet-loader";
import { getModuleSync, isAvailable, loadIpc } from "./predicate";
import {
  type Column,
  createTable,
  type TableData,
  type TableEngine,
} from "./table";
import { createWasmTableData } from "./wasm-table-data";
import "./style.css";

// --- Column definitions for the generated dataset ---

const generatedColumnOverrides: Record<string, Partial<Column>> = {
  id: { label: "ID", width: 90, sortable: true },
  name: { label: "Name", width: 180, sortable: true },
  location: { label: "Location", width: 180, sortable: true },
  department: { label: "Department", width: 160, sortable: true },
  note: { label: "Note", width: 300, sortable: false },
  status: { label: "Status", width: 120, sortable: true },
  priority: { label: "Priority", width: 100, sortable: true },
  score: { label: "Score", width: 100, sortable: true },
  email: { label: "Email", width: 200, sortable: true },
  verified: { label: "Verified", width: 100, sortable: true },
  joined: { label: "Joined", width: 120, sortable: true },
  chaos: { label: "Chaos", width: 130, sortable: true },
};

// --- State ---
let currentEngine: TableEngine | null = null;
let currentDatasetId = "generated";

// --- Reactive dataset selection ---
const dataset$ = new Subject<DatasetEntry>();

// --- Boot ---

function getInitialDataset(): string {
  const params = new URLSearchParams(window.location.search);
  return params.get("dataset") ?? "spotify";
}

function boot() {
  const app = document.getElementById("app")!;
  currentDatasetId = getInitialDataset();
  renderShell(app);

  // Wire up the reactive pipeline — switchMap cancels in-flight loads on dataset change
  dataset$
    .pipe(
      switchMap((dataset) => {
        const tableRoot = document.getElementById("table-root")!;

        // Clean up previous table before starting the new load
        if (currentEngine) {
          currentEngine.destroy();
          currentEngine = null;
        }

        // Update description and page title
        const descEl = document.getElementById("dataset-description");
        if (descEl) descEl.textContent = dataset.description;
        document.title = `Sift — ${dataset.label}`;

        renderLoadingSkeleton(tableRoot, "Loading data…");

        const load$ =
          dataset.source === "local"
            ? loadLocalArrow$(dataset, tableRoot)
            : loadHuggingFaceWasm$(dataset, tableRoot);

        return load$.pipe(
          catchError((err) => {
            console.error("Failed to load dataset:", err);
            tableRoot.innerHTML = `<div class="pt-loading">
            Failed to load dataset: ${err instanceof Error ? err.message : String(err)}
          </div>`;
            return EMPTY;
          }),
        );
      }),
    )
    .subscribe();

  // Kick off the initial dataset load
  const initial = DATASETS.find((d) => d.id === currentDatasetId);
  if (initial) dataset$.next(initial);
}

function renderShell(app: HTMLElement) {
  const dataset =
    DATASETS.find((d) => d.id === currentDatasetId) ?? DATASETS[0];

  app.innerHTML = `
    <div class="pt-page">
      <div class="pt-intro">
        <p class="pt-eyebrow">Pretext × Arrow × Semiotic</p>
        <div class="pt-intro-row">
          <h1>Sift</h1>
          <div class="pt-dataset-picker">
            <select id="dataset-select">
              ${DATASETS.map(
                (d) => `
                <option value="${d.id}" ${d.id === currentDatasetId ? "selected" : ""}>
                  ${d.label}${d.rows ? ` (${d.rows})` : ""}
                </option>
              `,
              ).join("")}
            </select>
          </div>
          <button class="pt-theme-toggle" id="theme-toggle" title="Toggle dark mode">◑</button>
          <a href="https://github.com/rgbkrk/sift" class="pt-github-btn" target="_blank" rel="noopener">
            <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>
            Star on GitHub
          </a>
        </div>
        <p class="pt-subtitle" id="dataset-description">${dataset.description}</p>
      </div>
      <div id="table-root"></div>
    </div>
  `;

  document.getElementById("dataset-select")!.addEventListener("change", (e) => {
    const select = e.target as HTMLSelectElement;
    const newId = select.value;
    if (newId !== currentDatasetId) {
      currentDatasetId = newId;
      // Update URL without reload
      const url = new URL(window.location.href);
      if (newId === "generated") {
        url.searchParams.delete("dataset");
      } else {
        url.searchParams.set("dataset", newId);
      }
      window.history.pushState({}, "", url);

      // Push to the reactive pipeline — switchMap handles cancellation
      const entry = DATASETS.find((d) => d.id === newId);
      if (entry) dataset$.next(entry);
    }
  });

  // Theme toggle: simple light ↔ dark
  const root = document.documentElement;
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const savedTheme = localStorage.getItem("theme");
  const initialDark = savedTheme ? savedTheme === "dark" : prefersDark;
  if (initialDark) root.setAttribute("data-theme", "dark");
  else root.setAttribute("data-theme", "light");

  document.getElementById("theme-toggle")!.addEventListener("click", () => {
    const isDark = root.getAttribute("data-theme") === "dark";
    root.setAttribute("data-theme", isDark ? "light" : "dark");
    localStorage.setItem("theme", isDark ? "light" : "dark");
  });
}

// --- Observable loaders ---

/**
 * Load a local Arrow file as an observable stream.
 * First emission mounts the table, subsequent emissions append batches.
 */
function loadLocalArrow$(
  dataset: DatasetEntry,
  tableRoot: HTMLElement,
): Observable<void> {
  return defer(
    () =>
      new Observable<void>((subscriber) => {
        let cancelled = false;

        (async () => {
          const response = await fetch(
            `${import.meta.env.BASE_URL}${dataset.path}`,
          );
          if (!response.ok) {
            tableRoot.innerHTML =
              '<div class="pt-loading">Missing data.arrow — run <code>npm run generate</code> first.</div>';
            subscriber.complete();
            return;
          }

          renderLoadingSkeleton(tableRoot, "Loading data…");

          // Start WASM init in parallel with Arrow download
          const [arrowBytes, wasmOk] = await Promise.all([
            response.arrayBuffer().then((b) => new Uint8Array(b)),
            isAvailable(),
          ]);

          if (cancelled) return;

          if (!wasmOk) {
            // WASM unavailable — fall back to JS path
            await loadLocalArrowJs$(
              dataset,
              tableRoot,
              subscriber,
              () => cancelled,
            );
            return;
          }

          renderLoadingSkeleton(tableRoot, "Loading into WASM…");
          const handle = await loadIpc(arrowBytes);

          const { tableData, columns, prefetchViewport } = createWasmTableData(
            handle,
            generatedColumnOverrides,
          );
          tableData.prefetchViewport = prefetchViewport;
          const mod = getModuleSync();
          tableData.recomputeSummaries = () =>
            updateWasmSummaries(mod, handle, tableData, columns);

          // Compute initial summaries
          updateWasmSummaries(mod, handle, tableData, columns);

          if (cancelled) return;
          tableRoot.innerHTML = "";
          currentEngine = createTable(tableRoot, tableData);
          currentEngine.setStreamingDone();
          subscriber.next();
          subscriber.complete();
        })().catch((err) => {
          if (!cancelled) subscriber.error(err);
        });

        return () => {
          cancelled = true;
        };
      }),
  );
}

/**
 * JS fallback for local Arrow loading (used when WASM is unavailable).
 */
async function loadLocalArrowJs$(
  dataset: DatasetEntry,
  tableRoot: HTMLElement,
  subscriber: import("rxjs").Subscriber<void>,
  isCancelled: () => boolean,
) {
  const response = await fetch(`${import.meta.env.BASE_URL}${dataset.path}`);
  const reader = await RecordBatchReader.from(response);
  await reader.open();

  const { columns, fieldNames, stringCols, rawCols, accumulators, tableData } =
    buildTableState(reader.schema, dataset, generatedColumnOverrides);

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
  if (isCancelled()) return;
  if (firstResult.done) {
    tableRoot.innerHTML =
      '<div class="pt-loading">No data in Arrow file.</div>';
    subscriber.complete();
    return;
  }
  appendBatch(firstResult.value);

  tableRoot.innerHTML = "";
  currentEngine = createTable(tableRoot, tableData);
  subscriber.next();

  for await (const batch of reader) {
    if (isCancelled()) return;
    appendBatch(batch);
    currentEngine!.onBatchAppended();
    subscriber.next();
  }

  currentEngine!.setStreamingDone();
  subscriber.complete();
}

/**
 * Load a HuggingFace Parquet dataset as an observable stream.
 * Emits once per row group. First emission mounts the table, the rest append.
 */
function loadHuggingFaceWasm$(
  dataset: DatasetEntry,
  tableRoot: HTMLElement,
): Observable<void> {
  // Outer defer ensures fresh execution per subscription (important for switchMap re-subscribe)
  return defer(
    () =>
      new Observable<void>((subscriber) => {
        let cancelled = false;

        (async () => {
          renderLoadingSkeleton(tableRoot, "Loading dataset…");

          // Start WASM init in parallel with data fetch
          const wasmInitPromise = isAvailable();

          // Try local cache first, fall back to HuggingFace
          const localUrl = `${import.meta.env.BASE_URL}datasets/${dataset.id}.parquet`;
          let resp: Response | null = null;

          if (cancelled) return;
          try {
            const localResp = await fetch(localUrl);
            const ct = localResp.headers.get("content-type") ?? "";
            if (localResp.ok && ct.includes("octet-stream")) resp = localResp;
          } catch {
            /* local cache miss, fall back to HF */
          }

          if (!resp) {
            if (cancelled) return;
            renderLoadingSkeleton(tableRoot, "Resolving dataset…");
            const url = await resolveHuggingFaceParquetUrl(
              dataset.path,
              dataset.config,
            );
            if (cancelled) return;
            renderLoadingSkeleton(tableRoot, "Downloading Parquet…");
            resp = await fetch(url);
          }

          const wasmOk = await wasmInitPromise;
          if (!wasmOk) {
            throw new Error("Failed to load nteract-predicate WASM module");
          }
          if (!resp.ok) {
            throw new Error(
              `Failed to fetch Parquet: ${resp.status} ${resp.statusText}`,
            );
          }
          const parquetBytes = new Uint8Array(await resp.arrayBuffer());

          if (cancelled) return;
          renderLoadingSkeleton(tableRoot, "Loading into WASM…");
          const mod = getModuleSync();

          // Get metadata to know how many row groups
          const meta = mod.parquet_metadata(parquetBytes);
          const numRowGroups = meta[0];
          const totalRows = meta[1];

          // Read schema metadata for pandas index_columns and HuggingFace feature types
          let schemaMetadata: Record<string, string> = {};
          try {
            schemaMetadata = mod.parquet_schema_metadata(
              parquetBytes,
            ) as Record<string, string>;
          } catch {
            /* metadata extraction is best-effort */
          }

          // Parse pandas index columns
          const pandasIndexCols = new Set<string>();
          if (schemaMetadata.pandas) {
            try {
              const pandas = JSON.parse(schemaMetadata.pandas);
              for (const ic of pandas.index_columns ?? []) {
                if (typeof ic === "string") pandasIndexCols.add(ic);
                // Range index descriptors don't map to a named column
              }
            } catch {
              /* ignore parse errors */
            }
          }

          // Parse HuggingFace feature metadata
          const hfFeatures: Record<
            string,
            { _type: string; names?: string[] }
          > = {};
          if (schemaMetadata.huggingface) {
            try {
              const hf = JSON.parse(schemaMetadata.huggingface);
              Object.assign(hfFeatures, hf?.info?.features ?? {});
            } catch {
              /* ignore parse errors */
            }
          }

          // Load first row group → mount table immediately
          const handle = mod.load_parquet_row_group(parquetBytes, 0, 0);

          const { tableData, columns, prefetchViewport } =
            createWasmTableData(handle);
          tableData.prefetchViewport = prefetchViewport;
          tableData.recomputeSummaries = () =>
            updateWasmSummaries(
              mod,
              handle,
              tableData,
              columns,
              pandasIndexCols,
            );

          // Apply metadata: mark pandas index columns, narrow index columns
          const isIndexName = (name: string) =>
            /^(unnamed[: _]*\d*|index|_?id|rowid|row_?id|row_?num)$/i.test(
              name,
            );
          for (const col of columns) {
            if (pandasIndexCols.has(col.key) || isIndexName(col.key)) {
              // Size to fit the max row number — estimate from digit count
              const digits = totalRows.toLocaleString().length;
              col.width = Math.max(60, digits * 9 + 24); // ~9px per char + cell padding
              col.sortable = false;
              // Hide labels for pandas artifacts — not real column names users would query
              if (/^(unnamed[: _]*\d*|__index_level_\d+__)$/i.test(col.key))
                col.label = "";
            }
            const hfFeature = hfFeatures[col.key];
            if (
              hfFeature?._type === "ClassLabel" &&
              col.columnType !== "categorical"
            ) {
              // HF says this is a classification label — treat as categorical
              col.columnType = "categorical";
              col.numeric = false;
            }
          }

          // Compute initial summaries from first row group
          updateWasmSummaries(mod, handle, tableData, columns, pandasIndexCols);

          if (cancelled) return;
          tableRoot.innerHTML = "";
          currentEngine = createTable(tableRoot, tableData);
          subscriber.next();

          // Stream remaining row groups progressively
          for (let g = 1; g < numRowGroups; g++) {
            if (cancelled) return;
            // Yield to the event loop so the UI stays responsive
            await new Promise((r) => setTimeout(r, 0));
            if (cancelled) return;
            mod.load_parquet_row_group(parquetBytes, g, handle);
            tableData.rowCount = mod.num_rows(handle);
            updateWasmSummaries(
              mod,
              handle,
              tableData,
              columns,
              pandasIndexCols,
            );
            currentEngine!.onBatchAppended();
            subscriber.next();
          }

          currentEngine!.setStreamingDone();
          subscriber.complete();
        })().catch((err) => {
          if (!cancelled) subscriber.error(err);
        });

        return () => {
          cancelled = true;
        };
      }),
  );
}

/** Compute summaries from the WASM store and update tableData. */
function updateWasmSummaries(
  mod: ReturnType<typeof getModuleSync>,
  handle: number,
  tableData: TableData,
  columns: Column[],
  pandasIndexCols?: Set<string>,
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
        // Compute median text length across unique values
        const lengths = counts
          .map(({ label }) => label.length)
          .sort((a, b) => a - b);
        const medianTextLength =
          lengths.length > 0 ? lengths[Math.floor(lengths.length / 2)] : 0;

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
        const [trueCount, falseCount, nullCount] = mod.store_bool_counts(
          handle,
          c,
        );
        return {
          kind: "boolean" as const,
          trueCount,
          falseCount,
          nullCount,
          total: numRows,
        };
      }
      case "timestamp": {
        // Use temporal binning: auto-detects granularity (hourly/daily/monthly/yearly)
        const bins = mod.store_temporal_histogram(handle, c) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        const totalInBins = bins.reduce((s, b) => s + b.count, 0);
        const nullCount = numRows - totalInBins;
        return {
          kind: "timestamp" as const,
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
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
        const totalInBins = bins.reduce((s, b) => s + b.count, 0);
        const nullCount = numRows - totalInBins;
        const summary: {
          kind: "numeric";
          min: number;
          max: number;
          bins: typeof bins;
          uniqueCount?: number;
          nullCount?: number;
        } = {
          kind: "numeric",
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
          nullCount: nullCount > 0 ? nullCount : undefined,
        };
        const nonZeroBins = bins.filter((b) => b.count > 0).length;
        // Detect low cardinality: count non-zero bins as a proxy for unique values
        if (nonZeroBins <= 10) {
          summary.uniqueCount = nonZeroBins;
        }
        // Detect index/ID columns: pandas metadata or name pattern.
        const isPandasIndex = pandasIndexCols?.has(col.key) ?? false;
        const isIndexName =
          /^(unnamed[: _]*\d*|index|_?id|rowid|row_?id|row_?num)$/i.test(
            col.key,
          );
        if (isPandasIndex || isIndexName) {
          (summary as any).isIndex = true;
        }
        return summary;
      }
    }
  });
}

function renderLoadingSkeleton(tableRoot: HTMLElement, status: string) {
  const existing = tableRoot.querySelector(".pt-skeleton");
  if (existing) {
    const statusEl = existing.querySelector(".pt-skeleton-status");
    if (statusEl) statusEl.textContent = status;
    return;
  }
  tableRoot.innerHTML = `
    <div class="pt-skeleton">
      <div class="pt-skeleton-header">
        ${Array.from({ length: 6 }, () => '<div class="pt-skeleton-th"><div class="pt-skeleton-bar pt-skeleton-label"></div><div class="pt-skeleton-bar pt-skeleton-chart"></div></div>').join("")}
      </div>
      <div class="pt-skeleton-body">
        ${Array.from({ length: 12 }, () => `<div class="pt-skeleton-row">${Array.from({ length: 6 }, () => '<div class="pt-skeleton-cell"><div class="pt-skeleton-bar pt-skeleton-text"></div></div>').join("")}</div>`).join("")}
      </div>
      <div class="pt-skeleton-footer">
        <span class="pt-skeleton-status">${status}</span>
      </div>
    </div>
  `;
}

// Old JS/parquet-wasm HuggingFace loader removed — WASM path handles everything now.
// See loadHuggingFaceWasm$() above.

// Kept for reference: type refinement logic (string→timestamp, null sentinels)
/** Build table columns, stores, and accumulators from an Arrow schema. */
function buildTableState(
  schema: import("apache-arrow").Schema,
  dataset: DatasetEntry,
  columnOverrides?: Record<string, Partial<Column>>,
) {
  const fieldNames = schema.fields.map((f) => f.name);
  const typeOverrides = dataset.typeOverrides ?? {};

  const columns: Column[] = schema.fields.map((field) => {
    const colType = typeOverrides[field.name] ?? detectColumnType(field);
    const overrides = columnOverrides?.[field.name];
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

boot();
