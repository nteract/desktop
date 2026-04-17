/* tslint:disable */
/* eslint-disable */

/**
 * Cast a column to a different type in-place.
 * Supported casts: string→timestamp (parse ISO dates), string→numeric, etc.
 * Uses arrow-cast for type conversion. Updates the store's column type metadata.
 * Saves the original column data so it can be restored when casting back.
 */
export function cast_column(handle: number, col: number, target_type: string): void;

/**
 * Get column names as a JSON array.
 */
export function col_names(handle: number): any;

/**
 * Get the detected type of a column ("numeric", "categorical", "boolean", "timestamp").
 */
export function col_type(handle: number, col: number): string;

/**
 * Filter rows by a boolean mask and return filtered Arrow IPC bytes.
 *
 * Takes: Arrow IPC bytes, boolean mask as Uint8Array (0/1 per row)
 * Returns: Filtered Arrow IPC bytes
 */
export function filter_rows(ipc_bytes: Uint8Array, mask: Uint8Array): Uint8Array;

/**
 * Free a loaded dataset from WASM memory.
 */
export function free(handle: number): void;

/**
 * Get a cell value as f64 (for numeric sorting/comparison). Returns NaN for non-numeric or null.
 */
export function get_cell_f64(handle: number, row: number, col: number): number;

/**
 * Get a cell value as a formatted string (for display).
 */
export function get_cell_string(handle: number, row: number, col: number): string;

/**
 * Get a viewport slice as Arrow IPC bytes.
 * Returns the rows [start_row, end_row) serialized as Arrow IPC stream.
 * This is the hot-path function — one call per scroll frame.
 */
export function get_viewport(handle: number, start_row: number, end_row: number): Uint8Array;

/**
 * Get a viewport slice for specific rows by index (for sorted/filtered views).
 * `indices` is a Uint32Array of row indices to fetch.
 * Returns Arrow IPC bytes containing those specific rows in order.
 */
export function get_viewport_by_indices(handle: number, indices: Uint32Array): Uint8Array;

/**
 * Check if a column has been cast (i.e. original data is saved and can be restored).
 */
export function has_original_column(handle: number, col: number): boolean;

/**
 * Compute a histogram (binned counts) for a numeric column.
 *
 * Takes: Arrow IPC bytes, column index, number of bins
 * Returns: JSON array of { x0, x1, count }
 */
export function histogram(ipc_bytes: Uint8Array, column_index: number, num_bins: number): any;

/**
 * Initialize the WASM module. Call once before using other functions.
 * Sets up panic hook so Rust panics show readable messages in the browser console.
 */
export function init(): void;

/**
 * Check if a cell is null.
 */
export function is_null(handle: number, row: number, col: number): boolean;

/**
 * Load Arrow IPC bytes into WASM memory. Returns a handle for subsequent operations.
 */
export function load_ipc(ipc_bytes: Uint8Array): number;

/**
 * Load Parquet bytes into WASM memory. Returns a handle for subsequent operations.
 * This replaces the need for parquet-wasm — one WASM binary for everything.
 */
export function load_parquet(parquet_bytes: Uint8Array): number;

/**
 * Load a single Parquet row group into a new or existing store.
 * If handle is 0, creates a new store and returns the handle.
 * If handle is non-zero, appends the row group to the existing store.
 */
export function load_parquet_row_group(parquet_bytes: Uint8Array, row_group: number, handle: number): number;

/**
 * Get the number of columns in a loaded dataset.
 */
export function num_cols(handle: number): number;

/**
 * Get the number of rows in a loaded dataset.
 */
export function num_rows(handle: number): number;

/**
 * Get Parquet metadata: number of row groups and total rows.
 * Returns [num_row_groups, total_rows] as Vec<u32>.
 */
export function parquet_metadata(parquet_bytes: Uint8Array): Uint32Array;

/**
 * Extract key-value metadata from a Parquet file's schema.
 * Returns a JSON object with metadata keys like "pandas", "huggingface", etc.
 */
export function parquet_schema_metadata(parquet_bytes: Uint8Array): any;

/**
 * Count boolean values in a column: returns [true_count, false_count, null_count].
 */
export function store_bool_counts(handle: number, col: number): Uint32Array;

/**
 * Apply filter predicates to the store and return matching row indices.
 * `filters_js` is a JSON array of filter specs:
 *   [{kind: "range", col: 0, min: 10, max: 50},
 *    {kind: "set", col: 1, values: ["a", "b"]},
 *    {kind: "boolean", col: 3, value: true}]
 * Returns a Vec<u32> of row indices that pass ALL filters (AND logic).
 */
export function store_filter_rows(handle: number, filters_js: any): Uint32Array;

/**
 * Filtered bool counts: returns [true_count, false_count, null_count] for masked rows.
 */
export function store_filtered_bool_counts(handle: number, col: number, mask: Uint8Array): Uint32Array;

/**
 * Filtered histogram: computes bins only for rows where mask[row] != 0.
 */
export function store_filtered_histogram(handle: number, col: number, mask: Uint8Array, num_bins: number): any;

/**
 * Filtered value_counts: counts string values only for rows where mask[row] != 0.
 */
export function store_filtered_value_counts(handle: number, col: number, mask: Uint8Array): any;

/**
 * Compute histogram for a numeric column in a loaded store.
 */
export function store_histogram(handle: number, col: number, num_bins: number): any;

/**
 * Sort a column and return sorted row indices.
 * `ascending`: true for asc, false for desc.
 * Nulls are always sorted to the end.
 */
export function store_sort_indices(handle: number, col: number, ascending: boolean): Uint32Array;

/**
 * Compute temporal histogram: bins timestamps by calendar unit (auto-detected).
 * Granularity: <48h → hourly, <90d → daily, <3y → monthly, else yearly.
 * Returns bins with x0/x1 as epoch milliseconds.
 */
export function store_temporal_histogram(handle: number, col: number): any;

/**
 * Compute value_counts for a column in a loaded store. Much faster than
 * the JS accumulator path since it iterates batches in Rust.
 */
export function store_value_counts(handle: number, col: number): any;

/**
 * Search a string column for values containing a substring.
 * Returns indices of matching rows as a Uint32Array.
 *
 * Takes: Arrow IPC bytes, column index, search query
 * Returns: Array of matching row indices
 */
export function string_contains(ipc_bytes: Uint8Array, column_index: number, query: string): Uint32Array;

/**
 * Undo a column cast, restoring the original column data and type.
 * Returns the original column type string (e.g. "categorical", "numeric").
 */
export function undo_cast_column(handle: number, col: number): string;

/**
 * Compute a frequency table (value_counts) for a string column
 * passed as Arrow IPC bytes.
 *
 * Takes: Arrow IPC bytes containing a single string/dictionary column
 * Returns: JSON array of { label, count } sorted by count descending
 */
export function value_counts(ipc_bytes: Uint8Array, column_index: number): any;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly cast_column: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly col_names: (a: number, b: number) => void;
    readonly col_type: (a: number, b: number, c: number) => void;
    readonly filter_rows: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly free: (a: number) => void;
    readonly get_cell_f64: (a: number, b: number, c: number, d: number) => void;
    readonly get_cell_string: (a: number, b: number, c: number, d: number) => void;
    readonly get_viewport: (a: number, b: number, c: number, d: number) => void;
    readonly get_viewport_by_indices: (a: number, b: number, c: number, d: number) => void;
    readonly has_original_column: (a: number, b: number, c: number) => void;
    readonly histogram: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly init: () => void;
    readonly is_null: (a: number, b: number, c: number, d: number) => void;
    readonly load_ipc: (a: number, b: number, c: number) => void;
    readonly load_parquet: (a: number, b: number, c: number) => void;
    readonly load_parquet_row_group: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly num_cols: (a: number, b: number) => void;
    readonly num_rows: (a: number, b: number) => void;
    readonly parquet_metadata: (a: number, b: number, c: number) => void;
    readonly parquet_schema_metadata: (a: number, b: number, c: number) => void;
    readonly store_bool_counts: (a: number, b: number, c: number) => void;
    readonly store_filter_rows: (a: number, b: number, c: number) => void;
    readonly store_filtered_bool_counts: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly store_filtered_histogram: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly store_filtered_value_counts: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly store_histogram: (a: number, b: number, c: number, d: number) => void;
    readonly store_sort_indices: (a: number, b: number, c: number, d: number) => void;
    readonly store_temporal_histogram: (a: number, b: number, c: number) => void;
    readonly store_value_counts: (a: number, b: number, c: number) => void;
    readonly string_contains: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly undo_cast_column: (a: number, b: number, c: number) => void;
    readonly value_counts: (a: number, b: number, c: number, d: number) => void;
    readonly rust_zstd_wasm_shim_calloc: (a: number, b: number) => number;
    readonly rust_zstd_wasm_shim_free: (a: number) => void;
    readonly rust_zstd_wasm_shim_malloc: (a: number) => number;
    readonly rust_zstd_wasm_shim_memcmp: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memcpy: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memmove: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memset: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_qsort: (a: number, b: number, c: number, d: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
