# Streaming Arrow IPC for DataFrame repr

**Status:** Draft
**Date:** 2026-04-22
**Related issues:** #1816 (this spec), #1815 (query backend — same transport), #1905 / #1911 / #1913 (per-task stable actors that make fork+merge on async paths safe), [2026-04-19 addressable execution outputs](./2026-04-19-map-keyed-outputs.md)
**Post-spec changes:** RuntimeStateDoc moved to `runtime-doc` crate (#2056). All CRDT writes go through `RuntimeStateHandle` (#2059) with `with_doc()` for sync and `fork()`/`merge()` for async. Dead broadcasts removed (#2065) — the pull task's manifest updates propagate via CRDT sync, not broadcasts.
**Related code:** `python/dx/src/dx/_format.py`, `python/dx/src/dx/_format_install.py`, `crates/runtimed/src/output_store.rs`, `crates/runtimed/src/output_prep.rs`, `crates/runtime-doc/src/doc.rs`, `crates/sift-wasm/src/store.rs`, `packages/sift/src/wasm-table-data.ts`

## Context

When a user evaluates a DataFrame in a cell (`df = pl.read_parquet(path); df`), dx serializes the entire frame to a single Parquet blob up front. For millions of rows the first paint is delayed by the full serialize → blob-store → sync → frontend hop, even though the user typically only needs the first screenful of rows.

This spec defines a streaming emission path: dx emits a small Parquet preview (the "head") so the existing sift load path lights up immediately, then the kernel continues to emit Arrow IPC record batches that append to the same output over time. The runtime agent stays a messenger — it doesn't understand Arrow semantics, it just routes chunks through the existing CRDT write machinery.

The transport is deliberately shared with #1815 (interactive query backend). A "streaming DataFrame handle" is a first-class concept that both the repr path and the query path produce; this spec establishes the handle and its first producer.

### Non-goals

- **Query pushdown.** Filter/sort/aggregate against the full table on the kernel side is #1815. This spec only covers "emit the rows forward."
- **Mutable blobs.** The blob store stays write-once and content-addressed. Streaming is expressed as a growing list of blob refs, not a growing blob.
- **Replacing Parquet.** The head stays Parquet. The continuation is Arrow IPC. Sift already handles both.

## Decisions that shape the design

These are settled based on the brainstorming session — they're load-bearing for everything below.

1. **The head stays Parquet.** A small number of rows (default 100; see [Tuning knobs](#tuning-knobs)) ships as the existing dx Parquet path so sift's existing load stays on its hot path and metadata discovery is unchanged.
2. **The continuation is Arrow IPC stream format.** Record-batch-per-chunk. Chunks are byte-sized (target ~1 MB per batch; see tuning knobs), not row-sized, so latency is predictable regardless of column width.
3. **Streaming goes through the CRDT, not a side channel.** Late joiners replay from the start. The runtime agent applies each new chunk as a CRDT mutation on the same output id, so all peers converge.
4. **The runtime agent owns the emission loop, not the kernel.** dx returns the head + a pull handle; the runtime agent iterates the handle on its own time, after the cell has synchronously returned. The cell's busy spinner clears at the head. No kernel-side threads.
5. **One logical output per DataFrame.** The output is addressed by its stable `output_id` (per [map-keyed outputs](./2026-04-19-map-keyed-outputs.md)). As chunks arrive, the runtime agent updates the same manifest in place, growing an ordered list of Arrow blob refs under a new MIME entry.

## Architecture

```
┌──────────────────────┐     ┌─────────────────────────┐     ┌─────────────────────┐
│ dx (in IPython kernel) │   │ runtime agent           │     │ frontend (sift)     │
│                      │     │                         │     │                     │
│ df = ...             │     │ sees "streaming handle" │     │ watches output      │
│ repr(df) →           │───→ │ on first display_data:  │     │ manifests by id     │
│   parquet head       │     │  1. head → blob store   │───→ │                     │
│   + pull handle      │     │     → manifest append   │     │ parquet head loads  │
│                      │     │  2. loop, pulling       │     │ into sift store     │
│ pull → arrow batch   │←──→ │     arrow batches       │     │                     │
│ pull → arrow batch   │←──→ │     → blob store        │     │ arrow batches       │
│ ...                  │     │     → manifest update   │───→ │ load_parquet_row_   │
│ EOF                  │     │     (same output_id)    │     │ group-style append  │
└──────────────────────┘     └─────────────────────────┘     └─────────────────────┘
```

One new MIME, one new comm target, one new manifest field. No changes to ContentRef, no changes to blob store. Everything else composes from existing machinery.

## Components

### 1. dx — emit head + pull handle

dx's existing `_format_install.py` already serializes the full frame to Parquet and attaches the buffer via ZMQ `send(..., buffers=...)`. Two changes:

- **Head-only Parquet.** For streaming-eligible frames (> threshold rows; see [Tuning knobs](#tuning-knobs)), serialize only `df.head(N)` as Parquet. This is fast and keeps metadata/schema/dtype detection working on the frontend exactly as today.
- **Pull handle.** Alongside the head, dx returns a handle — an id the runtime agent can use to request subsequent Arrow batches. The handle is advertised via a new top-level comm in the reserved `nteract.dx.stream` namespace (see `CLAUDE.md` § Reserved Comm Namespace). The comm's `open` message carries `{handle_id, total_rows, schema, rows_remaining_estimate}`. The comm is daemon-facing only — per the namespace rule, the runtime agent filters it out of `RuntimeStateDoc.comms` so it never surfaces as a widget.

dx handles `comm_msg` on the stream comm. The protocol is three messages:

- **request:** `{op: "pull", budget: <bytes>}` — the agent asks for up to `budget` bytes worth of Arrow IPC.
- **chunk response:** `{op: "chunk", rows: <int>}` with a binary buffer containing Arrow IPC stream bytes (schema message on the first chunk, record-batch messages after).
- **done response:** `{op: "done"}` — no more rows. dx closes the comm after sending.

Errors get their own reply: `{op: "error", message: "..."}`, after which dx closes the comm.

The handle is bound to the frame's Python-side identity, not to the display message. If the user overwrites the variable, the handle's underlying reader stays valid until the comm closes or the kernel restarts.

**First emission path (single synchronous hop):**

```python
# in dx._format_install, roughly
if should_stream(df):
    head_bytes = serialize_parquet(df.head(HEAD_ROWS))
    handle_id = register_stream_handle(df)  # opens nteract.dx.stream.<id> comm
    bundle = {
        BLOB_REF_MIME: build_ref_bundle(...head blob ref..., content_type="application/vnd.apache.parquet"),
        STREAM_HANDLE_MIME: {"handle_id": handle_id, "total_rows": len(df), "schema": arrow_schema_json(df)},
        "text/llm+plain": llm_preview,
    }
    display_hook_ship(bundle, buffers=[head_bytes])
else:
    # existing single-shot path
    ...
```

`STREAM_HANDLE_MIME = "application/vnd.nteract.dataframe-stream-handle+json"` — a lightweight JSON description. No binary in this MIME.

### 2. Runtime agent — handle detection + pull loop

The agent already runs `preflight_ref_buffers` to extract the blob-ref buffer into the blob store (`output_store.rs:646-699`). Extend that path:

- When an incoming display_data carries `STREAM_HANDLE_MIME`, the agent records the handle on the just-created output manifest (stamped with `output_id` per the existing map-keyed output design) and spawns a **pull task** keyed by `output_id`.
- The pull task runs outside the execution-message hot path. It loops:
  1. Call `nteract.dx.stream.<handle_id>.pull(BATCH_BYTES)` via a comm_msg. dx returns one Arrow IPC chunk (bytes) or a "done" signal.
  2. If bytes: put them in the blob store (content-addressed, same as any other blob). Get back a hash + size.
  3. Append the new blob ref to the manifest's `STREAM_CHUNKS` entry via `replace_output(execution_id, output_idx, new_manifest)` using `RuntimeStateHandle::fork()`/`merge()` for the async blob-store work.
  4. Loop until dx signals done, the comm closes, the execution is cleared, or the cell is re-run.

The comm pull uses the same `nteract.dx.*` transport reserved in `CLAUDE.md`. Binary buffers flow directly to the blob store (the rule that lets these comms bypass `RuntimeStateDoc.comms` does the right thing by default).

**Manifest shape** (what the runtime agent writes):

```json
{
  "output_type": "display_data",
  "output_id": "uuid-v4",
  "data": {
    "application/vnd.nteract.blob-ref+json": { /* head parquet ref */ },
    "application/vnd.nteract.dataframe-stream-handle+json": { "handle_id": "...", "total_rows": 20_000_000, "schema": {...} },
    "application/vnd.nteract.arrow-stream-chunks+json": {
      "chunks": [
        {"blob": "sha256:...", "size": 1_048_576, "rows": 12_340},
        {"blob": "sha256:...", "size": 1_049_001, "rows": 12_515}
      ],
      "complete": false
    },
    "text/llm+plain": "..."
  },
  "metadata": {}
}
```

The `arrow-stream-chunks` entry is a JSON-inlined manifest (small: one entry per ~1 MB chunk). `complete: true` is set when dx signals done. This preserves the ContentRef invariant — each chunk is still one blob, the list of refs is metadata inside the manifest, not a new ContentRef variant.

### 3. Frontend — sift consumes chunks in order

`sift-wasm` already exposes `load_parquet` and `load_ipc`; the `DataStore` already supports batch append (`crates/sift-wasm/src/store.rs:1167-1205`). The frontend side is almost entirely plumbing:

- The DataFrame renderer watches the output manifest for `arrow-stream-chunks`. When it sees the head Parquet, it calls `load_parquet(head_bytes)` and creates a sift store.
- When new chunks appear in `arrow-stream-chunks.chunks`, the renderer fetches each via the blob HTTP server (existing), calls a new `load_ipc_append(store_handle, ipc_bytes)` on sift-wasm (lightweight extension of the existing load_parquet_row_group-style append), and triggers a re-render.
- Progress UI: "Loaded 124,855 / 20,000,000 rows — streaming…" driven by summing `rows` across `chunks` vs. `total_rows` from the handle.

Because the frontend reads manifests directly from the CRDT, late joiners and refreshes get the same incremental experience: they see however many chunks have been appended so far and keep receiving more as the runtime agent finishes the pull loop. If the agent has already finished, `complete: true` tells the renderer to stop expecting more.

## Data flow (single cell, happy path)

1. User runs `df`. Kernel calls dx's repr hook.
2. dx serializes `df.head(100)` to Parquet, opens a `nteract.dx.stream.<id>` comm, returns a display bundle with the Parquet blob-ref, the stream-handle MIME, and `text/llm+plain`.
3. Runtime agent's IOPub handler creates the output manifest (mints `output_id`), writes the Parquet head to the blob store, writes the manifest to the CRDT. **Cell busy spinner clears. First paint happens.**
4. Runtime agent notices the stream-handle MIME, spawns a pull task for `output_id`.
5. Pull task: `comm_msg({op: "pull", budget: 1_048_576})` → dx responds with ~1 MB Arrow IPC bytes.
6. Pull task: blob-store the bytes, then call `replace_output(execution_id, output_idx, new_manifest)` — which uses the existing fork+merge transaction helper — to append the new chunk ref to `arrow-stream-chunks.chunks`.
7. Frontend sees the manifest update, fetches the blob, appends to sift store, re-renders.
8. Loop (5–7) until dx returns `{op: "done"}`. Pull task sets `complete: true` on the manifest and exits.

## Error handling

- **Comm drops / kernel restart mid-stream.** Pull task detects the closed comm, sets `complete: false` and adds `{error: "stream_interrupted"}` to the manifest. The frontend renders whatever rows have arrived plus an "interrupted" banner. No retry — re-executing the cell produces a fresh stream.
- **dx error during pull** (e.g. Arrow encoding fails for an unusual dtype). dx returns `{op: "error", message: "..."}` on the comm. Runtime agent records the error on the manifest the same way, stops the pull task. Frontend shows the partial rows + error.
- **Blob store write failure.** Current behavior: output manifest records the failure per-MIME. Streaming extends this — one bad chunk sets `complete: false` + `error` and stops further pulls for that output.
- **Re-execution of the cell.** Existing `clear_execution_outputs` drops the whole list; the pull task for the old `output_id` sees its output vanish on the next CRDT read and exits. No dangling comm.

## Testing

- **Native (`cargo test`):** Unit tests on the manifest-mutation helpers. Drive the new `replace_output` flow with synthetic manifests to confirm `arrow-stream-chunks.chunks` grows correctly under fork+merge.
- **Python integration:** dx emits a known fixture DataFrame; the existing integration harness verifies head bytes, stream-handle contents, and pull-then-close sequence against a fake comm counterpart. Verifies the head-only Parquet doesn't serialize the full frame (size assertion: head bytes ≪ full bytes).
- **End-to-end (wdio in `packages/sift/e2e`):** Load a notebook that produces a 5 M-row frame; assert first paint happens before all chunks arrive (head renders before the pull loop completes), progress UI advances monotonically, final row count matches `total_rows`. A specific latency budget is out of scope for the spec — set it after a measurement pass on a reference machine during implementation.
- **Interruption:** Kill the kernel mid-stream; assert the frontend shows an interrupted banner and the partial data is still usable.
- **Regression for non-streaming frames:** Small frames (< `STREAM_THRESHOLD_ROWS`) take the existing single-shot path unchanged.

## Tuning knobs

Configurable via env vars or (eventually) user settings. Default values listed; these are starting points, not final values:

| Name | Default | Description |
|------|---------|-------------|
| `NTERACT_DX_STREAM_THRESHOLD_ROWS` | 10_000 | Frames smaller than this use the existing single-shot Parquet path. |
| `NTERACT_DX_STREAM_HEAD_ROWS` | 100 | Rows in the initial Parquet head. |
| `NTERACT_DX_STREAM_BATCH_BYTES` | 1_048_576 (1 MB) | Target bytes per Arrow chunk. dx picks a row count to approximate this per batch. |
| `NTERACT_DX_STREAM_MAX_CHUNKS` | 512 | Safety cap. If a frame would need more than this many chunks, stop streaming (render what's loaded + a "truncated" banner). Prevents pathological 100+ GB frames from flooding the blob store and the CRDT. |

## What this does not change

- **ContentRef shape.** Still two variants (`Inline`, `Blob`). The list-of-chunks concept lives inside the manifest as a JSON-inlined field, not as a new ContentRef variant.
- **Blob store semantics.** Still write-once, content-addressed. Each chunk is its own blob with its own hash.
- **`output_id` semantics.** Unchanged — still stamped once when the manifest is first emitted, stable across all chunk appends.
- **`display_index` semantics.** Unchanged — a streaming DataFrame with a `display_id` is still addressable via the index exactly as any other output.
- **Existing `preflight_ref_buffers` path.** The head still flows through it. Only the pull task is new code.
- **`RuntimeStateDoc.comms`.** The `nteract.dx.stream.*` comms are filtered out the same way `nteract.dx.blob` is today; they never appear as widgets.

## Open questions to resolve during implementation

1. **Threshold source.** Rows (as proposed) vs. estimated bytes (`len(df) * avg_row_bytes`)? Rows is simpler to reason about; bytes is more accurate for very wide tables.
2. **Pull concurrency per output.** Single-flight (one in-flight pull per output, next starts after the last write lands) vs. pipelined (up to K pulls in flight). Pipelined is faster but risks reordering. Start single-flight; revisit if throughput is limiting.
3. **Back-pressure.** If the frontend is scrolled to the top and the user isn't even looking at row 100k yet, should the agent slow down? Simplest answer: no — it's the user's machine, and finishing fast lets the GC compact everything. Worth a measurement pass.
4. **Handle lifetime in dx.** Should a handle survive cell re-execution if `df` is the same object? Or does every `repr(df)` mint a fresh handle? Fresh is simpler and matches "each display is independent." Go fresh.

## Review pointers

- `python/dx/src/dx/_format_install.py:420-437` — current Parquet emission (where the head-only branch goes).
- `crates/runtimed/src/output_store.rs:646-699` — `preflight_ref_buffers` (where the stream-handle MIME gets picked up).
- `crates/runtime-doc/src/doc.rs` — `replace_output` (used by the pull task to update the manifest per chunk). RuntimeStateDoc now lives in the `runtime-doc` crate, accessed via `RuntimeStateHandle::with_doc()` or `fork()`/`merge()` for async paths.
- `crates/sift-wasm/src/store.rs:1167-1205` — existing batch-append in `load_parquet_row_group` (shape to mirror for `load_ipc_append`).
- `packages/sift/src/wasm-table-data.ts:8,41-80` — where the streaming renderer hooks in.
- `docs/superpowers/specs/2026-04-19-map-keyed-outputs.md` — `output_id` stability guarantee that makes "update the same output" work.
- `CLAUDE.md` § Reserved Comm Namespace — why `nteract.dx.stream.*` comms are safe to use without polluting widget state.
