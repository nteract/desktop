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
     * Get outputs as a JSON array string of structured manifest objects.
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
     * Add a Pixi conda dependency (matchspec). Deduplicates by package name.
     */
    add_pixi_dependency(pkg: string): void;
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
     * Roll back sync state after a failed `flush_local_changes()` delivery.
     *
     * If the message from `flush_local_changes()` was NOT delivered to the
     * daemon (e.g. sendFrame failed, relay mutex blocked), call this to clear
     * `in_flight` and `sent_hashes`. Without this, `generate_sync_message`
     * will permanently filter out the change data for hashes it believes were
     * already sent, causing a protocol stall that only `reset_sync_state()`
     * (page reload) can recover from.
     *
     * Clearing `sent_hashes` may cause some change data to be resent on the
     * next sync message, but the protocol tolerates duplicates — Automerge's
     * `load_incremental` deduplicates on receive.
     */
    cancel_last_flush(): void;
    /**
     * Roll back pool sync state after a failed delivery.
     */
    cancel_last_pool_state_flush(): void;
    /**
     * Roll back runtime-state sync state after a failed
     * `flush_runtime_state_sync()` delivery.
     *
     * Mirrors `cancel_last_flush()` for the notebook doc: clears
     * `in_flight` and `sent_hashes` on `state_sync_state` so the next
     * `flush_runtime_state_sync()` or `generate_runtime_state_sync_reply()`
     * produces a message instead of returning `None`.
     */
    cancel_last_runtime_state_flush(): void;
    /**
     * Get the number of cells in the document.
     */
    cell_count(): number;
    /**
     * Clear the Conda section entirely.
     */
    clear_conda_section(): void;
    /**
     * Clear the Pixi section entirely.
     */
    clear_pixi_section(): void;
    /**
     * Clear the UV section entirely (deps + requires-python).
     */
    clear_uv_section(): void;
    /**
     * Return the deduplicated, sorted list of actor labels that have
     * contributed changes to this document's history.
     *
     * Useful for debugging provenance — call after sync to see which
     * peers (e.g., `"runtimed"`, `"human:abc123"`) have touched the notebook.
     */
    contributing_actors(): string[];
    /**
     * Create a bootstrap handle for sync — no notebook ID, just skeleton + encoding + actor.
     *
     * This is the preferred constructor for sync-only clients. The daemon
     * populates the full document via Automerge sync.
     */
    static create_bootstrap(actor_label: string): NotebookHandle;
    /**
     * Create a handle with the bootstrap skeleton for sync.
     *
     * Deprecated — use [`create_bootstrap()`](Self::create_bootstrap) which
     * requires an actor label.
     */
    static create_empty(): NotebookHandle;
    /**
     * Create a bootstrap handle with a specific actor identity.
     *
     * Deprecated — use [`create_bootstrap()`](Self::create_bootstrap).
     */
    static create_empty_with_actor(actor_label: string): NotebookHandle;
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
     * Flush any pending local changes as a sync message to send to the daemon.
     *
     * Call this after local CRDT mutations (cell edits, metadata changes) to
     * push them to the daemon. Returns the message as a byte array, or
     * `undefined` if there are no unsent local changes.
     *
     * This is the ONLY way to generate an outbound sync message besides the
     * reply embedded in `receive_frame()`. Having exactly two controlled paths
     * (reply-to-inbound and flush-local) prevents the consumption race from
     * #1067 where `flushSync` and `syncReply$` both called
     * `generate_sync_message`, racing on the shared `sync_state`.
     *
     * If the returned message cannot be delivered, the caller MUST call
     * `cancel_last_flush()` to prevent `sent_hashes` from permanently
     * filtering out the undelivered change data.
     */
    flush_local_changes(): Uint8Array | undefined;
    /**
     * Generate an initial PoolDoc sync message.
     *
     * Call this during bootstrap so the daemon syncs pool state.
     */
    flush_pool_state_sync(): Uint8Array | undefined;
    /**
     * Generate an initial RuntimeStateDoc sync message.
     *
     * Call this during bootstrap (alongside `flush_local_changes` for the
     * notebook doc) so the daemon knows we need the full RuntimeStateDoc.
     * Without this, if the daemon's initial `RuntimeStateSync` frame arrives
     * before the WASM handle is ready, the kernel status is never synced
     * and the frontend stays stuck on "not_started".
     *
     * If the returned message cannot be delivered, the caller MUST call
     * `cancel_last_runtime_state_flush()` to prevent `sent_hashes` from
     * permanently filtering out the undelivered state data.
     */
    flush_runtime_state_sync(): Uint8Array | undefined;
    /**
     * Generate a sync reply for the PoolDoc.
     */
    generate_pool_state_sync_reply(): Uint8Array | undefined;
    /**
     * Generate a sync reply for the RuntimeStateDoc.
     * Called immediately after each `RuntimeStateSyncApplied` event
     * so the daemon knows which state the client has received.
     */
    generate_runtime_state_sync_reply(): Uint8Array | undefined;
    /**
     * Get the actor identity label for this document.
     */
    get_actor_id(): string;
    /**
     * Get a single cell by ID, or null if not found.
     */
    get_cell(cell_id: string): JsCell | undefined;
    /**
     * Get a cell's execution count.
     */
    get_cell_execution_count(cell_id: string): string | undefined;
    /**
     * Get ordered cell IDs (sorted by position, tiebreak on ID).
     */
    get_cell_ids(): string[];
    /**
     * Get a cell's metadata as a native JS object.
     *
     * Returns undefined if the cell doesn't exist.
     */
    get_cell_metadata(cell_id: string): any;
    /**
     * Get a cell's outputs as a native JS array of manifest objects.
     *
     * Each element is a structured output manifest (with MIME bundles and
     * ContentRef blob/inline refs). Returns undefined if the cell doesn't exist.
     *
     * Outputs live in the RuntimeStateDoc keyed by execution_id. This method
     * reads the cell's `execution_id` from the notebook doc, then looks up
     * outputs in the state doc — the dedicated outputs lookup that replaces
     * the old "read the snapshot and inspect `snapshot.outputs`" path.
     */
    get_cell_outputs(cell_id: string): any;
    /**
     * Get a cell's fractional index position string.
     */
    get_cell_position(cell_id: string): string | undefined;
    /**
     * Get a cell's source text.
     */
    get_cell_source(cell_id: string): string | undefined;
    /**
     * Get a cell's type — "code", "markdown", or "raw".
     */
    get_cell_type(cell_id: string): string | undefined;
    /**
     * Get all cells as an array of JsCell objects.
     *
     * Outputs are fetched from `RuntimeStateDoc` keyed by each cell's
     * `execution_id`. Cells without an execution_id or with empty outputs
     * return an empty outputs vec.
     */
    get_cells(): JsCell[];
    /**
     * Get all cells as a JSON string (for bulk materialization).
     *
     * Serializes the same shape as `get_cells()` but as a single JSON
     * string — cheaper to cross the WASM boundary than many individual
     * property getters. Outputs are fetched from `RuntimeStateDoc` keyed
     * by each cell's `execution_id`; `CellSnapshot` itself no longer
     * carries outputs.
     */
    get_cells_json(): string;
    /**
     * Get a metadata value by key (legacy string API).
     */
    get_metadata(key: string): string | undefined;
    /**
     * Return a stable fingerprint of the notebook metadata.
     *
     * Returns a cached JSON string suitable for equality comparison.
     * The cache is invalidated in `receive_frame` when the Automerge
     * doc actually changes (heads differ) and on all local metadata
     * mutation methods.
     *
     * Returns undefined if no metadata is present.
     */
    get_metadata_fingerprint(): string | undefined;
    /**
     * Get the full typed metadata as a native JS object.
     *
     * Returns the `NotebookMetadataSnapshot` as a JS object via serde-wasm-bindgen,
     * avoiding JSON string round-trips. Returns undefined if no metadata is set.
     */
    get_metadata_snapshot(): any;
    /**
     * Get the full typed metadata as a JSON string.
     *
     * Returns the `NotebookMetadataSnapshot` serialized as JSON, or undefined
     * if no metadata is set. The frontend can parse this with a shared TS interface.
     */
    get_metadata_snapshot_json(): string | undefined;
    /**
     * Get a metadata value as a native JS value.
     *
     * Reads the Automerge metadata subtree and returns it as a JS object/array/scalar.
     * Returns undefined if the key doesn't exist.
     */
    get_metadata_value(key: string): any;
    /**
     * Read the current pool state snapshot from the WASM doc.
     */
    get_pool_state(): any;
    /**
     * Read the current runtime state snapshot from the WASM doc.
     */
    get_runtime_state(): any;
    /**
     * Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
     */
    static load(bytes: Uint8Array): NotebookHandle;
    /**
     * Load a RuntimeStateDoc from saved bytes.
     *
     * Used by test fixtures to provide pre-populated state doc data
     * (outputs, executions) alongside the notebook doc.
     */
    load_state_doc(bytes: Uint8Array): void;
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
     * (no JSON string intermediate). Sync frames return a single `sync_applied`
     * event with an optional `CellChangeset` and an optional `reply`.
     *
     * **Sync replies are generated atomically** within this method after applying
     * each inbound `AUTOMERGE_SYNC` frame. The reply bytes (if any) are returned
     * in `FrameEvent::SyncApplied.reply` — the caller should send them immediately
     * via `sendFrame(0x00, reply)`. This eliminates the consumption race from #1067
     * where a separate `generate_sync_reply()` call could be preempted by
     * `flushSync`'s `generate_sync_message()`, both competing on the same
     * `sync_state`.
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
     * Remove a Pixi conda dependency by package name.
     * Returns true if a dependency was removed.
     */
    remove_pixi_dependency(pkg: string): boolean;
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
     * Resolve ContentRef values in a comm's state for frontend consumption.
     *
     * Walks the state **recursively**, resolving ContentRef objects:
     * - `{"blob": hash, "size": N, "media_type": M?}` → plain URL string
     * - `{"inline": value}` → unwrapped inner value
     * - Plain values → passed through unchanged
     *
     * Returns `{ state, buffer_paths, text_paths }`:
     * - `buffer_paths` — JSON paths of blob refs with binary MIME types (or no
     *   media_type). The caller fetches these as ArrayBuffers for ipywidgets
     *   buffer handling.
     * - `text_paths` — JSON paths of blob refs whose `media_type` classifies
     *   as text (`text/*`, `application/json`, `application/javascript`, etc.).
     *   The caller must fetch each URL, decode as UTF-8, and replace the URL
     *   string at that path with the decoded content before handing the state
     *   to widget code. Widgets that consume synced string traits (e.g.
     *   anywidget `_py_render`) expect the actual content, not a URL.
     *
     * Returns undefined if blob_port is not set or comm doesn't exist.
     */
    resolve_comm_state(comm_id: string): any;
    /**
     * Export the full document as bytes (for debugging or persistence).
     */
    save(): Uint8Array;
    /**
     * Set the actor identity for this document.
     *
     * Tags all subsequent edits with this label for provenance tracking.
     */
    set_actor(actor_label: string): void;
    /**
     * Set the blob server port for resolving binary ContentRefs to URLs.
     * Call after init and whenever the daemon restarts with a new port.
     */
    set_blob_port(port: number): void;
    /**
     * Replace entire cell metadata (last-write-wins).
     *
     * Accepts metadata as a JSON object string.
     * Returns true if the cell was found and updated.
     */
    set_cell_metadata(cell_id: string, metadata_json: string): boolean;
    /**
     * Replace entire cell metadata from a JS object (native, no JSON string).
     */
    set_cell_metadata_value(cell_id: string, metadata: any): boolean;
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
     * Set the cell tags from a JS array (native, no JSON string).
     *
     * Accepts a JS array of strings directly via serde-wasm-bindgen.
     */
    set_cell_tags_value(cell_id: string, tags: any): boolean;
    /**
     * Set multiple properties in a comm's state map at once.
     *
     * Accepts a JSON object string of key-value pairs to write.
     * Used by anywidget's `save_changes()` which batches pending mutations.
     * Call `flush_runtime_state_sync()` after to propagate.
     */
    set_comm_state_batch(comm_id: string, patch_json: string): boolean;
    /**
     * Set a single property in a comm's state map.
     *
     * Writes directly to `comms/{comm_id}/state/{key}` as a native
     * Automerge value. Call `flush_runtime_state_sync()` after mutations
     * to propagate changes to the daemon.
     */
    set_comm_state_property(comm_id: string, key: string, value_json: string): boolean;
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
     * Set a metadata value (legacy string API).
     */
    set_metadata(key: string, value: string): void;
    /**
     * Set the full typed metadata snapshot from a JS object.
     *
     * Accepts a JS object matching the `NotebookMetadataSnapshot` shape and writes
     * it as native Automerge types (maps, lists, scalars). This enables per-field
     * CRDT merging instead of last-write-wins on a JSON string.
     */
    set_metadata_snapshot_value(value: any): void;
    /**
     * Set a metadata value from a JS object (native Automerge types).
     *
     * Accepts any JS value and writes it as native Automerge types under the
     * given key in the metadata map. Objects become Maps, arrays become Lists,
     * and scalars become native scalars.
     */
    set_metadata_value(key: string, value: any): void;
    /**
     * Set the MIME type priority list for output selection.
     * Types earlier in the list are preferred when narrowing output data bundles.
     * If empty, all MIME types are returned (backward compatible).
     */
    set_mime_priority(priority: any): void;
    /**
     * Set Pixi channels.
     * Accepts a JSON array string (e.g. `'["conda-forge"]'`).
     */
    set_pixi_channels(channels_json: string): void;
    /**
     * Set Pixi python version.
     * Pass undefined/null to clear the constraint.
     */
    set_pixi_python(python?: string | null): void;
    /**
     * Set UV prerelease strategy, preserving deps and requires-python.
     * Pass "allow", "disallow", "if-necessary", "explicit", "if-necessary-or-explicit", or null to clear.
     */
    set_uv_prerelease(prerelease?: string | null): void;
    /**
     * Set UV requires-python constraint, preserving deps.
     * Pass undefined/null to clear the constraint.
     */
    set_uv_requires_python(requires_python?: string | null): void;
    /**
     * Splice a cell's source at a specific position (character-level, no diff).
     */
    splice_source(cell_id: string, index: number, delete_count: number, text: string): boolean;
    /**
     * Update cell metadata at a specific path (e.g., ["jupyter", "source_hidden"]).
     *
     * Creates intermediate objects if they don't exist.
     * Accepts path and value as JSON strings.
     * Returns true if the cell was found and updated.
     */
    update_cell_metadata_at(cell_id: string, path_json: string, value_json: string): boolean;
    /**
     * Update cell metadata at a specific path using native JS values.
     *
     * Path is a JS array of strings, value is any JS value.
     * No JSON string round-trips.
     */
    update_cell_metadata_at_value(cell_id: string, path: any, value: any): boolean;
    /**
     * Update a cell's source text using Automerge Text CRDT (Myers diff).
     */
    update_source(cell_id: string, source: string): boolean;
}

/**
 * Encode a clear-channel message as a presence frame payload (CBOR).
 * Removes a single presence channel (e.g. cursor or selection) for this peer.
 */
export function encode_clear_channel_presence(peer_id: string, channel: string): Uint8Array;

/**
 * Encode a cursor position as a presence frame payload (CBOR).
 *
 * The frontend should prepend the frame type byte (0x04) and send
 * via `invoke("send_frame", { frameData })`.
 *
 * `peer_label` is the human-readable name shown in cursor flags
 * (e.g. the OS username). Pass an empty string to omit.
 */
export function encode_cursor_presence(peer_id: string, peer_label: string, actor_label: string, cell_id: string, line: number, column: number): Uint8Array;

/**
 * Encode a cell focus as a presence frame payload (CBOR).
 * Focus means "I'm on this cell" without an editor cursor position.
 */
export function encode_focus_presence(peer_id: string, peer_label: string, actor_label: string, cell_id: string): Uint8Array;

/**
 * Encode a selection range as a presence frame payload (CBOR).
 */
export function encode_selection_presence(peer_id: string, peer_label: string, actor_label: string, cell_id: string, anchor_line: number, anchor_col: number, head_line: number, head_col: number): Uint8Array;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_get_jscell_index: (a: number) => number;
    readonly __wbg_jscell_free: (a: number, b: number) => void;
    readonly __wbg_notebookhandle_free: (a: number, b: number) => void;
    readonly encode_clear_channel_presence: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly encode_cursor_presence: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => void;
    readonly encode_focus_presence: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly encode_selection_presence: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => void;
    readonly jscell_cell_type: (a: number, b: number) => void;
    readonly jscell_execution_count: (a: number, b: number) => void;
    readonly jscell_id: (a: number, b: number) => void;
    readonly jscell_metadata_json: (a: number, b: number) => void;
    readonly jscell_outputs_json: (a: number, b: number) => void;
    readonly jscell_position: (a: number, b: number) => void;
    readonly jscell_resolved_assets_json: (a: number, b: number) => void;
    readonly jscell_source: (a: number, b: number) => void;
    readonly notebookhandle_add_cell: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly notebookhandle_add_cell_after: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly notebookhandle_add_conda_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_add_pixi_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_add_uv_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_append_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_cancel_last_flush: (a: number) => void;
    readonly notebookhandle_cancel_last_pool_state_flush: (a: number) => void;
    readonly notebookhandle_cancel_last_runtime_state_flush: (a: number) => void;
    readonly notebookhandle_cell_count: (a: number) => number;
    readonly notebookhandle_clear_conda_section: (a: number, b: number) => void;
    readonly notebookhandle_clear_pixi_section: (a: number, b: number) => void;
    readonly notebookhandle_clear_uv_section: (a: number, b: number) => void;
    readonly notebookhandle_contributing_actors: (a: number, b: number) => void;
    readonly notebookhandle_create_bootstrap: (a: number, b: number) => number;
    readonly notebookhandle_create_empty: () => number;
    readonly notebookhandle_delete_cell: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_detect_runtime: (a: number, b: number) => void;
    readonly notebookhandle_flush_local_changes: (a: number, b: number) => void;
    readonly notebookhandle_flush_pool_state_sync: (a: number, b: number) => void;
    readonly notebookhandle_flush_runtime_state_sync: (a: number, b: number) => void;
    readonly notebookhandle_get_actor_id: (a: number, b: number) => void;
    readonly notebookhandle_get_cell: (a: number, b: number, c: number) => number;
    readonly notebookhandle_get_cell_execution_count: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_cell_ids: (a: number, b: number) => void;
    readonly notebookhandle_get_cell_metadata: (a: number, b: number, c: number) => number;
    readonly notebookhandle_get_cell_outputs: (a: number, b: number, c: number) => number;
    readonly notebookhandle_get_cell_position: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_cell_source: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_cell_type: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_cells: (a: number, b: number) => void;
    readonly notebookhandle_get_cells_json: (a: number, b: number) => void;
    readonly notebookhandle_get_metadata: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_get_metadata_fingerprint: (a: number, b: number) => void;
    readonly notebookhandle_get_metadata_snapshot: (a: number) => number;
    readonly notebookhandle_get_metadata_snapshot_json: (a: number, b: number) => void;
    readonly notebookhandle_get_metadata_value: (a: number, b: number, c: number) => number;
    readonly notebookhandle_get_pool_state: (a: number) => number;
    readonly notebookhandle_get_runtime_state: (a: number) => number;
    readonly notebookhandle_load: (a: number, b: number, c: number) => void;
    readonly notebookhandle_load_state_doc: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_move_cell: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_new: (a: number, b: number) => number;
    readonly notebookhandle_receive_frame: (a: number, b: number, c: number) => number;
    readonly notebookhandle_receive_sync_message: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_remove_conda_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_remove_pixi_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_remove_uv_dependency: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_reset_sync_state: (a: number) => void;
    readonly notebookhandle_resolve_comm_state: (a: number, b: number, c: number) => number;
    readonly notebookhandle_save: (a: number, b: number) => void;
    readonly notebookhandle_set_actor: (a: number, b: number, c: number) => void;
    readonly notebookhandle_set_blob_port: (a: number, b: number) => void;
    readonly notebookhandle_set_cell_metadata: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_set_cell_metadata_value: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_cell_outputs_hidden: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_cell_source_hidden: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_cell_tags: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_set_cell_tags_value: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_comm_state_batch: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly notebookhandle_set_comm_state_property: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly notebookhandle_set_conda_channels: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_conda_python: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_metadata: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_set_metadata_snapshot_value: (a: number, b: number, c: number) => void;
    readonly notebookhandle_set_metadata_value: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly notebookhandle_set_mime_priority: (a: number, b: number) => void;
    readonly notebookhandle_set_pixi_channels: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_pixi_python: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_uv_prerelease: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_set_uv_requires_python: (a: number, b: number, c: number, d: number) => void;
    readonly notebookhandle_splice_source: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly notebookhandle_update_cell_metadata_at: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly notebookhandle_update_cell_metadata_at_value: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_update_source: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly notebookhandle_generate_pool_state_sync_reply: (a: number, b: number) => void;
    readonly notebookhandle_generate_runtime_state_sync_reply: (a: number, b: number) => void;
    readonly notebookhandle_create_empty_with_actor: (a: number, b: number) => number;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
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
