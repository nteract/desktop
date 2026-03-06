/* @ts-self-types="./runtimed_wasm.d.ts" */

/**
 * A cell snapshot returned to JavaScript.
 */
export class JsCell {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(JsCell.prototype);
        obj.__wbg_ptr = ptr;
        JsCellFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        JsCellFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_jscell_free(ptr, 0);
    }
    /**
     * @returns {number}
     */
    get index() {
        const ret = wasm.__wbg_get_jscell_index(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {string}
     */
    get cell_type() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.jscell_cell_type(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    get execution_count() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.jscell_execution_count(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    get id() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.jscell_id(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Get outputs as a JSON array string.
     * @returns {string}
     */
    get outputs_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.jscell_outputs_json(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    get source() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.jscell_source(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
}
if (Symbol.dispose) JsCell.prototype[Symbol.dispose] = JsCell.prototype.free;

/**
 * A handle to a local Automerge notebook document.
 *
 * All mutations (add cell, delete cell, edit source) happen locally
 * and produce sync messages that the Tauri relay forwards to the daemon.
 * Incoming sync messages from the daemon are applied here, and the
 * frontend re-reads cells to update React state.
 */
export class NotebookHandle {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(NotebookHandle.prototype);
        obj.__wbg_ptr = ptr;
        NotebookHandleFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        NotebookHandleFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_notebookhandle_free(ptr, 0);
    }
    /**
     * Add a new cell at the given index.
     * @param {number} index
     * @param {string} cell_id
     * @param {string} cell_type
     */
    add_cell(index, cell_id, cell_type) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(cell_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(cell_type, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len1 = WASM_VECTOR_LEN;
            wasm.notebookhandle_add_cell(retptr, this.__wbg_ptr, index, ptr0, len0, ptr1, len1);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Append text to a cell's source (optimized for streaming, no diff).
     * @param {string} cell_id
     * @param {string} text
     * @returns {boolean}
     */
    append_source(cell_id, text) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(cell_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(text, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len1 = WASM_VECTOR_LEN;
            wasm.notebookhandle_append_source(retptr, this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return r0 !== 0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Get the number of cells in the document.
     * @returns {number}
     */
    cell_count() {
        const ret = wasm.notebookhandle_cell_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Delete a cell by ID. Returns true if the cell was found and deleted.
     * @param {string} cell_id
     * @returns {boolean}
     */
    delete_cell(cell_id) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(cell_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            wasm.notebookhandle_delete_cell(retptr, this.__wbg_ptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return r0 !== 0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Generate a sync message to send to the relay peer.
     *
     * Returns the message as a byte array, or undefined if already in sync.
     * The caller should send these bytes via `invoke("send_automerge_sync", { syncMessage })`.
     * @returns {Uint8Array | undefined}
     */
    generate_sync_message() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.notebookhandle_generate_sync_message(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getArrayU8FromWasm0(r0, r1).slice();
                wasm.__wbindgen_export2(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Get a single cell by ID, or null if not found.
     * @param {string} cell_id
     * @returns {JsCell | undefined}
     */
    get_cell(cell_id) {
        const ptr0 = passStringToWasm0(cell_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.notebookhandle_get_cell(this.__wbg_ptr, ptr0, len0);
        return ret === 0 ? undefined : JsCell.__wrap(ret);
    }
    /**
     * Get all cells as an array of JsCell objects.
     * @returns {JsCell[]}
     */
    get_cells() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.notebookhandle_get_cells(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayJsValueFromWasm0(r0, r1).slice();
            wasm.__wbindgen_export2(r0, r1 * 4, 4);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Get all cells as a JSON string (for bulk materialization).
     * @returns {string}
     */
    get_cells_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.notebookhandle_get_cells_json(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export2(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Get a metadata value by key.
     * @param {string} key
     * @returns {string | undefined}
     */
    get_metadata(key) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            wasm.notebookhandle_get_metadata(retptr, this.__wbg_ptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v2;
            if (r0 !== 0) {
                v2 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export2(r0, r1 * 1, 1);
            }
            return v2;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Load a notebook document from saved bytes (e.g., from get_automerge_doc_bytes).
     * @param {Uint8Array} bytes
     * @returns {NotebookHandle}
     */
    static load(bytes) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_export3);
            const len0 = WASM_VECTOR_LEN;
            wasm.notebookhandle_load(retptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return NotebookHandle.__wrap(r0);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Create a new empty notebook document.
     * @param {string} notebook_id
     */
    constructor(notebook_id) {
        const ptr0 = passStringToWasm0(notebook_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.notebookhandle_new(ptr0, len0);
        this.__wbg_ptr = ret >>> 0;
        NotebookHandleFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Receive and apply a sync message from the relay peer.
     *
     * Returns true if the document changed (caller should re-read cells).
     * @param {Uint8Array} message
     * @returns {boolean}
     */
    receive_sync_message(message) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passArray8ToWasm0(message, wasm.__wbindgen_export3);
            const len0 = WASM_VECTOR_LEN;
            wasm.notebookhandle_receive_sync_message(retptr, this.__wbg_ptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return r0 !== 0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Reset the sync state. Call this when reconnecting to a new relay session.
     */
    reset_sync_state() {
        wasm.notebookhandle_reset_sync_state(this.__wbg_ptr);
    }
    /**
     * Export the full document as bytes (for debugging or persistence).
     * @returns {Uint8Array}
     */
    save() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.notebookhandle_save(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayU8FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export2(r0, r1 * 1, 1);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Set a metadata value.
     * @param {string} key
     * @param {string} value
     */
    set_metadata(key, value) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(value, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len1 = WASM_VECTOR_LEN;
            wasm.notebookhandle_set_metadata(retptr, this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Update a cell's source text using Automerge Text CRDT (Myers diff).
     * @param {string} cell_id
     * @param {string} source
     * @returns {boolean}
     */
    update_source(cell_id, source) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(cell_id, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(source, wasm.__wbindgen_export3, wasm.__wbindgen_export4);
            const len1 = WASM_VECTOR_LEN;
            wasm.notebookhandle_update_source(retptr, this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return r0 !== 0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) NotebookHandle.prototype[Symbol.dispose] = NotebookHandle.prototype.free;

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_4577686b3a6d9b3a: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg___wbindgen_throw_39bc967c0e5a9b58: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_getRandomValues_3f44b700395062e5: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_jscell_new: function(arg0) {
            const ret = JsCell.__wrap(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./runtimed_wasm_bg.js": import0,
    };
}

const JsCellFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_jscell_free(ptr >>> 0, 1));
const NotebookHandleFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_notebookhandle_free(ptr >>> 0, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayJsValueFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    const mem = getDataViewMemory0();
    const result = [];
    for (let i = ptr; i < ptr + 4 * len; i += 4) {
        result.push(takeObject(mem.getUint32(i, true)));
    }
    return result;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('runtimed_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
