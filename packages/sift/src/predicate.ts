/**
 * Lazy-loading wrapper for the nteract-predicate WASM module.
 *
 * The WASM binary is loaded on first use, so it doesn't affect
 * initial page load for users who don't need compute operations.
 */

/** Filter spec passed to WASM store_filter_rows. */
export type FilterSpecJson =
  | { kind: "range"; col: number; min: number; max: number }
  | { kind: "set"; col: number; values: string[] }
  | { kind: "boolean"; col: number; value: boolean };

type PredicateModule = {
  // Data store
  load_ipc(ipc_bytes: Uint8Array): number;
  load_parquet(parquet_bytes: Uint8Array): number;
  parquet_metadata(parquet_bytes: Uint8Array): Uint32Array;
  parquet_schema_metadata(parquet_bytes: Uint8Array): Record<string, string>;
  load_parquet_row_group(parquet_bytes: Uint8Array, row_group: number, handle: number): number;
  cast_column(handle: number, col: number, target_type: string): void;
  has_original_column(handle: number, col: number): boolean;
  undo_cast_column(handle: number, col: number): string;
  free(handle: number): void;
  num_rows(handle: number): number;
  num_cols(handle: number): number;
  col_names(handle: number): string[];
  col_type(handle: number, col: number): string;
  get_cell_string(handle: number, row: number, col: number): string;
  get_cell_f64(handle: number, row: number, col: number): number;
  is_null(handle: number, row: number, col: number): boolean;
  // Store-based summaries (operates on handle, iterates in Rust)
  store_value_counts(handle: number, col: number): { label: string; count: number }[];
  store_histogram(
    handle: number,
    col: number,
    num_bins: number,
  ): { x0: number; x1: number; count: number }[];
  store_temporal_histogram(
    handle: number,
    col: number,
  ): { x0: number; x1: number; count: number }[];
  store_bool_counts(handle: number, col: number): Uint32Array;
  store_sort_indices(handle: number, col: number, ascending: boolean): Uint32Array;
  // Store-based filtered summaries (crossfilter — takes byte mask)
  store_filtered_value_counts(
    handle: number,
    col: number,
    mask: Uint8Array,
  ): { label: string; count: number }[];
  store_filtered_histogram(
    handle: number,
    col: number,
    mask: Uint8Array,
    num_bins: number,
  ): { x0: number; x1: number; count: number }[];
  store_filtered_bool_counts(handle: number, col: number, mask: Uint8Array): Uint32Array;
  // Store-based filter (applies predicates in Rust, returns matching row indices)
  store_filter_rows(handle: number, filters: FilterSpecJson[]): Uint32Array;
  // Viewport access (returns Arrow IPC for visible rows)
  get_viewport(handle: number, start_row: number, end_row: number): Uint8Array;
  get_viewport_by_indices(handle: number, indices: Uint32Array): Uint8Array;
  // Compute (stateless, takes IPC bytes)
  value_counts(ipc_bytes: Uint8Array, column_index: number): { label: string; count: number }[];
  histogram(
    ipc_bytes: Uint8Array,
    column_index: number,
    num_bins: number,
  ): { x0: number; x1: number; count: number }[];
  filter_rows(ipc_bytes: Uint8Array, mask: Uint8Array): Uint8Array;
  string_contains(ipc_bytes: Uint8Array, column_index: number, query: string): Uint32Array;
};

let mod: PredicateModule | null = null;

async function ensureModule(): Promise<PredicateModule> {
  if (mod) return mod;
  const wasm = await import("nteract-predicate/nteract_predicate.js");
  await wasm.default();
  mod = wasm as unknown as PredicateModule;
  return mod;
}

/**
 * Search a string column for values containing a substring.
 * Returns indices of matching rows.
 */
export async function stringContains(
  ipcBytes: Uint8Array,
  columnIndex: number,
  query: string,
): Promise<Uint32Array> {
  const m = await ensureModule();
  return m.string_contains(ipcBytes, columnIndex, query);
}

/**
 * Compute value_counts for a string column.
 * Returns sorted array of { label, count }.
 */
export async function valueCounts(
  ipcBytes: Uint8Array,
  columnIndex: number,
): Promise<{ label: string; count: number }[]> {
  const m = await ensureModule();
  return m.value_counts(ipcBytes, columnIndex);
}

/**
 * Compute histogram bins for a numeric column.
 */
export async function histogram(
  ipcBytes: Uint8Array,
  columnIndex: number,
  numBins: number,
): Promise<{ x0: number; x1: number; count: number }[]> {
  const m = await ensureModule();
  return m.histogram(ipcBytes, columnIndex, numBins);
}

/**
 * Filter rows by a boolean mask, return filtered Arrow IPC bytes.
 */
export async function filterRows(ipcBytes: Uint8Array, mask: Uint8Array): Promise<Uint8Array> {
  const m = await ensureModule();
  return m.filter_rows(ipcBytes, mask);
}

/**
 * Check if the WASM module is available (built and loadable).
 */
/**
 * Check if the WASM module is available (built and loadable).
 */
export async function isAvailable(): Promise<boolean> {
  try {
    await ensureModule();
    return true;
  } catch {
    return false;
  }
}

// --- Data store (WASM-owned data) ---

/** Load Arrow IPC bytes into WASM memory. Returns a handle. */
export async function loadIpc(ipcBytes: Uint8Array): Promise<number> {
  const m = await ensureModule();
  return m.load_ipc(ipcBytes);
}

/** Load Parquet bytes into WASM memory. Returns a handle. Replaces parquet-wasm. */
export async function loadParquet(parquetBytes: Uint8Array): Promise<number> {
  const m = await ensureModule();
  return m.load_parquet(parquetBytes);
}

/** Free a loaded dataset from WASM memory. */
export async function free(handle: number): Promise<void> {
  const m = await ensureModule();
  m.free(handle);
}

/** Get the number of rows in a loaded dataset. */
export async function numRows(handle: number): Promise<number> {
  const m = await ensureModule();
  return m.num_rows(handle);
}

/** Get the number of columns. */
export async function numCols(handle: number): Promise<number> {
  const m = await ensureModule();
  return m.num_cols(handle);
}

/** Get column names. */
export async function colNames(handle: number): Promise<string[]> {
  const m = await ensureModule();
  return m.col_names(handle);
}

/** Get detected column type. */
export async function colType(handle: number, col: number): Promise<string> {
  const m = await ensureModule();
  return m.col_type(handle, col);
}

/** Get a cell value formatted as string. */
export async function getCellString(handle: number, row: number, col: number): Promise<string> {
  const m = await ensureModule();
  return m.get_cell_string(handle, row, col);
}

/** Get a cell value as f64 (NaN for non-numeric or null). */
export async function getCellF64(handle: number, row: number, col: number): Promise<number> {
  const m = await ensureModule();
  return m.get_cell_f64(handle, row, col);
}

/** Check if a cell is null. */
export async function isNull(handle: number, row: number, col: number): Promise<boolean> {
  const m = await ensureModule();
  return m.is_null(handle, row, col);
}

// --- Synchronous access (only valid after ensureModule() has resolved) ---

/**
 * Get the module reference synchronously. Throws if not yet initialized.
 * Call ensureModule() first during your async setup phase.
 */
export function getModuleSync(): PredicateModule {
  if (!mod) throw new Error("nteract-predicate WASM not initialized. Call ensureModule() first.");
  return mod;
}
