/* tslint:disable */
/* eslint-disable */

/**
 * A cell snapshot returned to JavaScript.
 */
export class JsCell {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Index in the sorted cell list (for backward compatibility).
     */
    readonly index: number;
    readonly cell_type: string;
    readonly execution_count: string;
    readonly id: string;
    /**
     * Get metadata as a JSON object string.
     */
    readonly metadata_json: string;
    /**
     * Get outputs as a JSON array string.
     */
    readonly outputs_json: string;
    /**
     * Fractional index hex string for ordering (e.g., "80", "7F80").
     */
    readonly position: string;
    /**
     * Get resolved asset refs as a JSON object string (`ref` → blob hash).
     */
    readonly resolved_assets_json: string;
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
     * Add a new cell at the given index (backward-compatible API).
     *
     * Internally converts the index to an after_cell_id for fractional indexing.
     */
    add_cell(index: number, cell_id: string, cell_type: string): void;
    /**
     * Add a new cell after the specified cell (semantic API).
     *
     * - `after_cell_id = null` → insert at the beginning
     * - `after_cell_id = "id"` → insert after that cell
     *
     * Returns the position string of the new cell.
     */
    add_cell_after(cell_id: string, cell_type: string, after_cell_id?: string | null): string;
    /**
     * Add a Conda dependency, deduplicating by package name (case-insensitive).
     * Initializes the Conda section with ["conda-forge"] channels if absent.
     */
    add_conda_dependency(pkg: string): void;
    /**
     * Add a UV dependency, deduplicating by package name (case-insensitive).
     * Initializes the UV section if absent, preserving existing fields.
     */
    add_uv_dependency(pkg: string): void;
    /**
     * Append text to a cell's source (optimized for streaming, no diff).
     */
    append_source(cell_id: string, text: string): boolean;
    /**
     * Get the number of cells in the document.
     */
    cell_count(): number;
    /**
     * Clear the Conda section entirely.
     */
    clear_conda_section(): void;
    /**
     * Clear the UV section entirely (deps + requires-python).
     */
    clear_uv_section(): void;
    /**
     * Create a handle with an empty Automerge doc (zero operations) for
     * sync-only bootstrap.  The sync protocol populates the doc from the
     * daemon — no `GetDocBytes` needed.
     */
    static create_empty(): NotebookHandle;
    /**
     * Delete a cell by ID. Returns true if the cell was found and deleted.
     */
    delete_cell(cell_id: string): boolean;
    /**
     * Detect the notebook runtime from kernelspec/language_info metadata.
     *
     * Returns "python", "deno", or undefined for unknown runtimes.
     */
    detect_runtime(): string | undefined;
    /**
     * Generate a sync message to send to the daemon (via the Tauri relay pipe).
     *
     * Returns the message as a byte array, or undefined if already in sync.
     * The caller should prepend the frame type byte (0x00 for AutomergeSync)
     * and send via `invoke("send_frame", { frameData })`.
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
     * Get the full typed metadata as a JSON string.
     *
     * Returns the `NotebookMetadataSnapshot` serialized as JSON, or undefined
     * if no metadata is set. The frontend can parse this with a shared TS interface.
     */
    get_metadata_snapshot_json(): string | undefined;
    /**
     * Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
     */
    static load(bytes: Uint8Array): NotebookHandle;
    /**
     * Move a cell to a new position (after the specified cell).
     *
     * - `after_cell_id = null` → move to the beginning
     * - `after_cell_id = "id"` → move after that cell
     *
     * This only updates the cell's position field — no delete/re-insert.
     * Returns the new position string.
     */
    move_cell(cell_id: string, after_cell_id?: string | null): string;
    /**
     * Create a new empty notebook document.
     */
    constructor(notebook_id: string);
    /**
     * Receive a typed frame from the daemon, demux by type byte, return events for the frontend.
     *
     * The input is the raw frame bytes from the `notebook:frame` Tauri event:
     * `[frame_type_byte, ...payload]`.
     *
     * Returns a JS array of `FrameEvent` objects directly via `serde-wasm-bindgen`
     * (no JSON string intermediate). Usually one event, but sync frames may produce
     * both a `sync_applied` and a `sync_reply` if the local doc needs to send a
     * response.
     *
     * When a `SyncReply` event is returned, its `reply` field contains raw
     * Automerge sync bytes (no frame type prefix). The frontend must prepend
     * the frame type byte (`0x00` for AutomergeSync) to form a complete typed
     * frame, then send it back via `invoke("send_frame", { frameData })`.
     *
     * Returns `undefined` if the frame is empty or cannot be processed.
     */
    receive_frame(frame_bytes: Uint8Array): any;
    /**
     * Receive and apply a sync message from the daemon (via the Tauri relay pipe).
     *
     * Returns true if the document changed (caller should re-read cells).
     */
    receive_sync_message(message: Uint8Array): boolean;
    /**
     * Remove a Conda dependency by package name (case-insensitive).
     * Returns true if a dependency was removed.
     */
    remove_conda_dependency(pkg: string): boolean;
    /**
     * Remove a UV dependency by package name (case-insensitive).
     * Returns true if a dependency was removed.
     */
    remove_uv_dependency(pkg: string): boolean;
    /**
     * Reset the sync state. Call this when reconnecting to a new daemon session.
     */
    reset_sync_state(): void;
    /**
     * Export the full document as bytes (for debugging or persistence).
     */
    save(): Uint8Array;
    /**
     * Replace entire cell metadata (last-write-wins).
     *
     * Accepts metadata as a JSON object string.
     * Returns true if the cell was found and updated.
     */
    set_cell_metadata(cell_id: string, metadata_json: string): boolean;
    /**
     * Set whether the cell outputs should be hidden (JupyterLab convention).
     *
     * Sets `metadata.jupyter.outputs_hidden` for the specified cell.
     * Returns true if the cell was found and updated.
     */
    set_cell_outputs_hidden(cell_id: string, hidden: boolean): boolean;
    /**
     * Set whether the cell source should be hidden (JupyterLab convention).
     *
     * Sets `metadata.jupyter.source_hidden` for the specified cell.
     * Returns true if the cell was found and updated.
     */
    set_cell_source_hidden(cell_id: string, hidden: boolean): boolean;
    /**
     * Set the cell tags.
     *
     * Accepts a JSON array string (e.g. `'["hide-input", "parameters"]'`).
     * Returns true if the cell was found and updated.
     */
    set_cell_tags(cell_id: string, tags_json: string): boolean;
    /**
     * Set Conda channels, preserving deps and python.
     * Accepts a JSON array string (e.g. `'["conda-forge","bioconda"]'`).
     */
    set_conda_channels(channels_json: string): void;
    /**
     * Set Conda python version, preserving deps and channels.
     * Pass undefined/null to clear the constraint.
     */
    set_conda_python(python?: string | null): void;
    /**
     * Set a metadata value.
     */
    set_metadata(key: string, value: string): void;
    /**
     * Set UV requires-python constraint, preserving deps.
     * Pass undefined/null to clear the constraint.
     */
    set_uv_requires_python(requires_python?: string | null): void;
    /**
     * Update cell metadata at a specific path (e.g., ["jupyter", "source_hidden"]).
     *
     * Creates intermediate objects if they don't exist.
     * Accepts path and value as JSON strings.
     * Returns true if the cell was found and updated.
     */
    update_cell_metadata_at(cell_id: string, path_json: string, value_json: string): boolean;
    /**
     * Update a cell's source text using Automerge Text CRDT (Myers diff).
     */
    update_source(cell_id: string, source: string): boolean;
}

/**
 * Encode a cursor position as a presence frame payload (CBOR).
 *
 * The frontend should prepend the frame type byte (0x04) and send
 * via `invoke("send_frame", { frameData })`.
 */
export function encode_cursor_presence(peer_id: string, cell_id: string, line: number, column: number): Uint8Array;

/**
 * Encode a selection range as a presence frame payload (CBOR).
 */
export function encode_selection_presence(peer_id: string, cell_id: string, anchor_line: number, anchor_col: number, head_line: number, head_col: number): Uint8Array;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_notebookhandle_free: (a: number, b: number) => void;
    readonly __wbg_jscell_free: (a: number, b: number) => void;
    readonly __wbg_get_jscell_index: (a: number) => number;
    readonly jscell_id: (a: number, b: number) => void;
    readonly jscell_cell_type: (a: number, b: number) => void;
    readonly jscell_position: (a: number, b: number) => void;
    readonly jscell_source: (a: number, b: number) => void;
    readonly jscell_execution_count: (a: number, b: number) => void;
    readonly jscell_outputs_json: (a: number, b: number) => void;
    readonly jscell_metadata_json: (a: number, b: number) => void;
    readonly jscell_resolved_assets_json: (a: number, b: number) => void;
    readonly notebookhandle_new: (a: number, b: number) => number;
    readonly notebookhandle_create_empty: () => number;
    readonly notebookhandle_load: (a: number, b: number, c: number) => void;
    readonly notebookhandle_cell_count: (a: number) => number;
    readonly notebookhandle_get_cells: (a: number, b: number) => void;
    readonly notebookhandle_get_cells_json: (a: number, b: number) => void;
    readonly notebookhandle_get_cell: (a: number, b: number, c: number) => number;
    readonly notebookhandle_add_cell: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly notebookhandle_add_cell_after: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly notebookhandle_move_cell: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_delete_cell: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_update_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_append_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_get_metadata: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_metadata_snapshot_json: (a: number, b: number) => void;
    readonly notebookhandle_detect_runtime: (a: number, b: number) => void;
    readonly notebookhandle_set_metadata: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_set_cell_source_hidden: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_cell_outputs_hidden: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_cell_tags: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_update_cell_metadata_at: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly notebookhandle_set_cell_metadata: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_add_uv_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_remove_uv_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_clear_uv_section: (a: number, b: number) => void;
    readonly notebookhandle_set_uv_requires_python: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_add_conda_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_remove_conda_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_clear_conda_section: (a: number, b: number) => void;
    readonly notebookhandle_set_conda_channels: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_conda_python: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_generate_sync_message: (a: number, b: number) => void;
    readonly notebookhandle_receive_sync_message: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_save: (a: number, b: number) => void;
    readonly notebookhandle_reset_sync_state: (a: number) => void;
    readonly notebookhandle_receive_frame: (a: number, b: number, c: number) => number;
    readonly encode_cursor_presence: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly encode_selection_presence: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly __wbindgen_export: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export2: (a: number, b: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
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
