/* tslint:disable */
/* eslint-disable */

/**
 * A cell snapshot returned to JavaScript.
 */
export class JsCell {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    readonly index: number;
    readonly cell_type: string;
    readonly execution_count: string;
    readonly id: string;
    /**
     * Get outputs as a JSON array string.
     */
    readonly outputs_json: string;
    readonly source: string;
}

/**
 * A handle to a local Automerge notebook document.
 *
 * All mutations (add cell, delete cell, edit source) happen locally
 * and produce sync messages that the Tauri relay forwards to the daemon.
 * Incoming sync messages from the daemon are applied here, and the
 * frontend re-reads cells to update React state.
 */
export class NotebookHandle {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Add a new cell at the given index.
     */
    add_cell(index: number, cell_id: string, cell_type: string): void;
    /**
     * Append text to a cell's source (optimized for streaming, no diff).
     */
    append_source(cell_id: string, text: string): boolean;
    /**
     * Get the number of cells in the document.
     */
    cell_count(): number;
    /**
     * Delete a cell by ID. Returns true if the cell was found and deleted.
     */
    delete_cell(cell_id: string): boolean;
    /**
     * Generate a sync message to send to the relay peer.
     *
     * Returns the message as a byte array, or undefined if already in sync.
     * The caller should send these bytes via `invoke("send_automerge_sync", { syncMessage })`.
     */
    generate_sync_message(): Uint8Array | undefined;
    /**
     * Get a single cell by ID, or null if not found.
     */
    get_cell(cell_id: string): JsCell | undefined;
    /**
     * Get all cells as an array of JsCell objects.
     */
    get_cells(): JsCell[];
    /**
     * Get all cells as a JSON string (for bulk materialization).
     */
    get_cells_json(): string;
    /**
     * Get a metadata value by key.
     */
    get_metadata(key: string): string | undefined;
    /**
     * Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
     */
    static load(bytes: Uint8Array): NotebookHandle;
    /**
     * Create a new empty notebook document.
     */
    constructor(notebook_id: string);
    /**
     * Receive and apply a sync message from the relay peer.
     *
     * Returns true if the document changed (caller should re-read cells).
     */
    receive_sync_message(message: Uint8Array): boolean;
    /**
     * Reset the sync state. Call this when reconnecting to a new relay session.
     */
    reset_sync_state(): void;
    /**
     * Export the full document as bytes (for debugging or persistence).
     */
    save(): Uint8Array;
    /**
     * Set a metadata value.
     */
    set_metadata(key: string, value: string): void;
    /**
     * Update a cell's source text using Automerge Text CRDT (Myers diff).
     */
    update_source(cell_id: string, source: string): boolean;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_notebookhandle_free: (a: number, b: number) => void;
    readonly __wbg_jscell_free: (a: number, b: number) => void;
    readonly __wbg_get_jscell_index: (a: number) => number;
    readonly jscell_id: (a: number, b: number) => void;
    readonly jscell_cell_type: (a: number, b: number) => void;
    readonly jscell_source: (a: number, b: number) => void;
    readonly jscell_execution_count: (a: number, b: number) => void;
    readonly jscell_outputs_json: (a: number, b: number) => void;
    readonly notebookhandle_new: (a: number, b: number) => number;
    readonly notebookhandle_load: (a: number, b: number, c: number) => void;
    readonly notebookhandle_cell_count: (a: number) => number;
    readonly notebookhandle_get_cells: (a: number, b: number) => void;
    readonly notebookhandle_get_cells_json: (a: number, b: number) => void;
    readonly notebookhandle_get_cell: (a: number, b: number, c: number) => number;
    readonly notebookhandle_add_cell: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly notebookhandle_delete_cell: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_update_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_append_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_get_metadata: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_metadata: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_generate_sync_message: (a: number, b: number) => void;
    readonly notebookhandle_receive_sync_message: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_save: (a: number, b: number) => void;
    readonly notebookhandle_reset_sync_state: (a: number) => void;
    readonly __wbindgen_export: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export3: (a: number, b: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number, d: number) => number;
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
