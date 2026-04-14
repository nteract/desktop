# nteract/dx — Python → Blob Store for the Modern nteract

**Status:** Design
**Date:** 2026-04-13
**Related:** #1334 (SSH remote runtimes), #1307 (kernel sandboxing), #1759 (parquet summaries, sift-wasm)

## Motivation

Today, when a Python kernel wants to render a parquet dataset in nteract, it ships the raw bytes over ZeroMQ IOPub inside a `display_data` message:

```python
display({"application/vnd.apache.parquet": buf.getvalue()}, raw=True)
```

Megabytes travel through ZMQ IOPub frames, base64-encoded inside a JSON bundle, then get decoded and re-hashed in the runtime agent. The daemon has a blob store sitting right there with content-addressed storage and dedupe. The kernel is taking the long way around.

We already ship:

- A blob store in the daemon (`crates/runtimed/src/blob_store.rs::put`), HTTP-served with content-addressed URLs.
- Inline output manifests with `ContentRef` entries written to the CRDT via fork/merge by the runtime agent (`crates/runtimed/src/output_store.rs::create_manifest` + `crates/runtimed/src/jupyter_kernel.rs` IOPub handlers).
- Frontend WASM that resolves ContentRefs to current blob URLs at render time.
- A sift/parquet renderer with predicate pushdown (#1759) that pages and slices parquet client-side.
- Runtime agent subprocess architecture (#1333, #1431, #1433, #1449) with a kernel↔runtime-agent ZMQ channel and an runtime-agent↔daemon Automerge/notebook-protocol channel. Remote kernels (#1334) keep ZMQ local on the remote machine and tunnel the *runtime-agent↔daemon* socket — not ZMQ.

`nteract/dx` fills the missing piece: a Python library that lets a kernel hand bytes to the blob store **without** putting them on IOPub, plus a clean display surface on top.

## Goals

1. Eliminate the "raw bytes in IOPub" pattern for parquet, Arrow, images, and arbitrary binary payloads.
2. Ride the existing runtime-agent-in-the-loop architecture so remote kernels benefit without new transport.
3. Ship a user-facing API (`dx.display`, `dx.put`) that feels as natural as `IPython.display`.
4. Degrade gracefully in vanilla Jupyter and plain `python` — no superpowers, no exceptions.
5. Forward-compatible with the planned ZMQ-less, in-process runtime agent.
6. Let Python compute `text/llm+plain` summaries at the source (typed columns, cheap iteration), rather than re-deriving them server-side.

## Non-Goals (v1)

- Live kernel-side interactive query backend (Buckaroo-style round-trip). Deferred, **not foreclosed** — see Future: Interactive Query Backend for the reserved hook points.
- Append-streaming Arrow batches. Protocol leaves room for it (see Future: Streaming).
- `dx.attach(path)` convenience. Good idea, not v1. Listed under Future.
- Authenticated HTTP write endpoint on the blob server.
- A blob-write RPC on the runtime-agent↔daemon socket. Not needed for v1 (runtime agent has filesystem access to the blob store). Documented under Dependencies below as a prerequisite for remote kernels.
- Automatic formatter registration on `import`. Explicit `dx.install()` only.

## Architecture

### Data path (local kernel)

```
┌──────────────────────────────────┐       ┌──────────────────────────┐
│  Python kernel process           │       │  runtime agent           │
│                                  │       │                          │
│  import dx                       │       │  comm target:            │
│  dx.install()                    │       │    nteract.dx.blob       │
│                                  │       │  (filtered from          │
│  dx.display(df)                  │       │   CommDocEntry/CRDT)     │
│    1. serialize df → parquet     │       │                          │
│    2. compute llm summary (py)   │       │  on comm_msg:            │
│    3. comm_msg                   │──────▶│    BlobStore::put(bytes, │
│       target: nteract.dx.blob    │  ZMQ  │      content_type)       │
│       buffers: [parquet bytes]   │       │    → returns hash        │
│    4. receive ack                │◀──────│  ack: {hash}             │
│    5. publish_display_data({     │──────▶│  on IOPub display_data:  │
│         ref_mime: {hash, ct,     │       │    detect ref MIME,      │
│           summary_hints},        │       │    compose ContentRef    │
│         "text/llm+plain": "..."  │       │    (no re-upload),       │
│       })                         │       │    fork/merge inline     │
└──────────────────────────────────┘       │    manifest              │
                                           └──────────────────────────┘
                                                      │
                                                      ▼
                                           ┌──────────────────────────┐
                                           │  CRDT inline manifest    │
                                           │  (ContentRef, hash-based)│
                                           └──────────────────────────┘
                                                      │
                                                      ▼
                                           ┌──────────────────────────┐
                                           │  Frontend WASM resolves  │
                                           │  ContentRef → blob URL   │
                                           │  sift/parquet renderer   │
                                           └──────────────────────────┘
```

### Data path (remote kernel, #1334)

Kernel↔runtime-agent ZMQ is **local to the remote machine**. The comm flow is unchanged. The runtime agent, running remotely, currently has direct filesystem access to the blob store (`blob_root` passed at spawn, `runtime_agent.rs:67`). For a true SSH-tunneled remote setup where the runtime agent runs on a different host from the daemon, a `PutBlob` frame on the runtime-agent↔daemon notebook-protocol socket is needed. That is a **dependency**, not part of this spec — see Dependencies.

### Why a comm, not a new socket

- Kernel↔runtime-agent ZMQ already exists, is HMAC-authenticated via Jupyter session, and already handles comm routing. Zero new transport.
- Comms are the standard Jupyter mechanism for binary sidechannels (ipywidgets uses them).
- Forward-compatible: when the runtime agent moves in-process with IPython, the comm becomes a function call; `dx` does not change.
- The protocol contract lives at the comm-message layer; a dedicated sidecar socket remains a drop-in replacement later.

### Keeping the blob comm off the CRDT

The runtime agent currently persists comm messages to the RuntimeStateDoc via `RuntimeStateDoc::put_comm` and `merge_comm_state_delta` (`crates/notebook-doc/src/runtime_state.rs`). That path exists for ipywidgets/anywidget state.

**The `nteract.dx.blob` comm must be excluded**, along with a **reserved namespace for future dx subsystems**. The runtime agent's comm-open handler in `crates/runtimed/src/jupyter_kernel.rs` (~line 1240–1393) short-circuits for any target whose name matches the `nteract.dx.*` prefix (v1 uses `nteract.dx.blob`; reserved for later: `nteract.dx.query`, `nteract.dx.stream`). This keeps future dx subsystems off the CRDT by default — widget-state semantics stay untouched:

- The comm is opened and routed to the dx blob handler.
- It is **not** written into `CommDocEntry`.
- `comm_msg` deltas on this target are **not** merged into the CRDT; the buffers go straight to `BlobStore::put`.
- `comm_close` is a local cleanup; no CRDT touch.

This is critical — putting raw parquet buffers into `doc.comms/{id}/state` would be exactly the anti-pattern we're solving. The filter lives in one place: the comm-handling code-path, gated on target_name.

### Durability: hash-primary, URL-ephemeral

The blob server port is dynamic. We do not stabilize it. Instead:

- The ref MIME carries **hash + content_type (+ optional summary hints)**. No URL.
- The CRDT stores a ContentRef (hash-based) — same shape used today for inline binary outputs.
- Frontend WASM derives the current blob URL at render time.
- `dx.put()` returns `BlobRef(hash, size)`. Kernel-side code never needs a blob URL — the ref MIME carries only the hash, and the frontend derives the current URL at render time via `ContentRef` resolution. External tooling that genuinely needs a URL should call `runt daemon status --json` explicitly.

## Protocol

### Comm target: `nteract.dx.blob`

Opened once per kernel session by `dx.install()`. All uploads multiplex on this comm.

**Kernel → runtime agent (comm_msg, buffers carry raw bytes):**

```json
{
  "op": "put",
  "req_id": "uuid-per-request",
  "content_type": "application/vnd.apache.parquet"
}
```

**Agent → kernel (comm_msg):**

```json
{
  "op": "ack",
  "req_id": "uuid-per-request",
  "hash": "sha256:abc123...",
  "size": 104857600
}
```

Errors: `{"op": "err", "req_id", "code", "message"}`. Codes include `too_large`, `agent_unavailable`, `blob_store_error`.

Agent implementation calls `BlobStore::put(&buffer, &content_type)` directly — idempotent, dedupes by hash, current 100 MiB ceiling enforced by `MAX_BLOB_SIZE` in `blob_store.rs`.

### Blob-ref MIME

New MIME: `application/vnd.nteract.blob-ref+json`

```json
{
  "hash": "sha256:abc123...",
  "content_type": "application/vnd.apache.parquet",
  "size": 104857600,
  "summary": {
    "total_rows": 148820,
    "included_rows": 10000,
    "sampled": true,
    "sample_strategy": "head"
  },
  "query": null
}
```

The optional `query` field is reserved for Future: Interactive Query Backend. When null/absent, the ref is a static blob. When populated, it carries a `handle_id` and capabilities advertising that the renderer may send live queries via the `nteract.dx.query` comm. v1 always emits `null`.

`summary` is optional and renderer-advisory — it lets sift show "showing 10,000 of 148,820 rows (head sample)" without opening the parquet first. The authoritative metadata still lives in the parquet file itself (pyarrow supports key-value metadata in the schema); `summary` is a fast hint.

`dx.display(df)` emits a `display_data` bundle containing:

- `application/vnd.nteract.blob-ref+json` — the reference
- `text/llm+plain` — computed Python-side (schema, dtypes, shape, head/tail samples, null counts) — cheap, iterable, and benefits from Python's type context

The runtime agent recognizes the ref MIME, composes a ContentRef under `content_type`, and also stores the `text/llm+plain` as a sibling entry in the manifest (existing path already handles non-binary MIMEs inline). `repr-llm` server-side synthesis remains the fallback when dx is not in the loop; when dx provides `text/llm+plain`, the runtime agent uses it as-is.

## API Surface (v1)

```python
import dx
dx.install()  # called from our ipykernel bootstrap; no-op on bare import

# High-level display
dx.display(df)                    # pandas / polars → parquet → ref + llm summary

# Low-level primitive
ref = dx.put(some_bytes, content_type="image/png")
# ref.hash, ref.size

# Emit a ref as a display bundle directly (e.g. user already has bytes in hand)
dx.display_blob_ref(ref, summary=None)
```

No `dx.attach(path)` in v1 (see Future).

### `dx.install()` behavior

- Opens the `nteract.dx.blob` comm. If no ack within ~100 ms, installs the fallback (raw-bytes `display_data`, current behavior). `dx` remains callable; optimization simply doesn't engage.
- Registers IPython MIME formatters for `pandas.DataFrame` and (if importable) `polars.DataFrame` so bare identifiers at cell-end route through `dx.display`.
- Idempotent.

### Environment detection

At `install()` time:

1. ipykernel present? → try to open comm.
2. Comm target acknowledged by the runtime agent within timeout? → superpowers on.
3. Otherwise → fallback (raw-bytes display_data).

Plain `python` with no ipykernel: `install()` is a no-op, `display(df)` returns `repr(df)`, `put(...)` raises `dx.DxNoAgentError`.

### Serialization: graceful degradation

DataFrame → parquet goes through a best-available-encoder chain. Modern pandas 2.x uses Arrow-backed dtypes, but parquet write still needs a backing library.

**Priority for pandas DataFrames:**

1. `pyarrow` — `pyarrow.Table.from_pandas(df)` + `pyarrow.parquet.write_table(buf)` — preferred (fastest, richest metadata).
2. `df.to_parquet(buf, engine="fastparquet")` — fallback if pyarrow absent.
3. None available → fallback to CSV bytes with `content_type: text/csv` (or `repr(df)` and log a loud warning).

**Priority for polars DataFrames:**

1. `df.write_parquet(buf)` — polars native, always available when polars is.

The chain is encapsulated in `dx._format.serialize_dataframe(df) -> (bytes, content_type)`. Missing optional deps produce a structured warning at `install()` time listing which encoders are available.

**Note:** writing the serializer itself in Rust (via pyo3) was considered but rejected for v1 — each DataFrame library has its own in-process Arrow representation, and crossing the FFI boundary to a Rust encoder would require converting first anyway. Pure-Python use of each library's native writer is both simpler and faster.

### `text/llm+plain` generation (Python-side)

`dx._summary.summarize(df) -> str` produces the summary. Contents:

- Library (pandas / polars), shape, dtypes table (column → dtype).
- Null counts per column (cheap — already computed by most DataFrame libs).
- Small head/tail sample (configurable row limit, default 10 each).
- If the serialized parquet was sampled (not full), mention it: "showing 10,000 of 148,820 rows (head sample)".

Iteration is trivial in Python; the same logic would be costly to port to Rust against a hash-indexed parquet blob server-side. `repr-llm` stays the fallback for legacy `display(..., raw=True)` paths.

## Components

### `python/dx/` (new uv workspace member)

- `dx/__init__.py` — public API (`display`, `put`, `display_blob_ref`, `install`, `BlobRef`, exceptions).
- `dx/_comm.py` — comm open, request/response multiplexing by `req_id`, timeout, fallback.
- `dx/_format.py` — IPython formatter registration, DataFrame → parquet chain.
- `dx/_summary.py` — `text/llm+plain` generation.
- `dx/_refs.py` — `BlobRef` dataclass, ref-MIME construction.
- `dx/_env.py` — environment detection.

Added as workspace member in repo-root `pyproject.toml` alongside `runtimed`, `nteract`, `gremlin`.

### Agent (Rust)

Concrete integration points:

- **Comm handling** — new handler in `crates/runtimed/src/jupyter_kernel.rs` near the existing comm handling (~1240–1393). Target-name switch: `"nteract.dx.blob"` → dx handler; everything else → existing widget/comm path.
- **Blob write** — the dx handler extracts raw buffers from the comm_msg envelope, calls `BlobStore::put(bytes, content_type)` (`crates/runtimed/src/blob_store.rs:63`), returns hash in the ack comm_msg.
- **CRDT filter** — the dx handler does **not** call `RuntimeStateDoc::put_comm` or `merge_comm_state_delta`. An early-return gated on `target_name` in the widget-comm path enforces the exclusion; a test asserts that comm traffic on this target never lands in `doc.comms`.
- **Ref MIME in display_data** — `crates/runtimed/src/output_store.rs::create_manifest` gains a branch for `application/vnd.nteract.blob-ref+json`: parse hash, compose `ContentRef` under the wrapped `content_type` (no blob re-upload — the blob is already there by hash). Surrounding sibling entries (e.g., `text/llm+plain`) continue through the existing text-inline path.
- **Size limit** — dx uploads honor `MAX_BLOB_SIZE` (100 MiB) from `blob_store.rs`. Over-limit returns `op: "err", code: "too_large"`.

Output writes keep the fork/merge pattern used today at `jupyter_kernel.rs` ~937, ~983, ~1155 — explicit lock acquisition, fork, mutate, reacquire, merge. No helper change.

### Kernel bootstrap

- `kernel-launch` adds `import dx; dx.install()` to ipykernel startup (via `--IPKernelApp.exec_lines` or a startup file).
- `dx` is included as a bootstrap dep alongside `ipykernel` in the managed Python environments.

### Frontend / renderer

**No changes required for v1.** The ContentRef path and sift/parquet renderer already handle hash-based refs end-to-end. Summary-hint rendering ("showing N of M rows") can land later once we have the upstream data flowing.

## Data Flow: dx.display(df)

1. User executes `df` (last line) or `dx.display(df)`.
2. `dx._format.serialize_dataframe(df)` → `(parquet_bytes, "application/vnd.apache.parquet")`, with a sampling decision based on size heuristics.
3. `dx._summary.summarize(df)` → `text/llm+plain` string.
4. `dx._comm` sends `comm_msg {op: "put", req_id, content_type}` + buffers.
5. Agent hashes, `BlobStore::put`, replies `{op: "ack", req_id, hash, size}`.
6. `dx` awaits ack (default 30 s), calls `IPython.display.publish_display_data` with `{ref_mime: {hash, content_type, size, summary?}, "text/llm+plain": "..."}`.
7. Agent receives IOPub display_data, `output_store::create_manifest` recognizes ref MIME, composes ContentRef in the inline manifest alongside the llm summary; fork/merge.
8. CRDT sync → frontend → WASM resolves ContentRef → sift/parquet renderer loads blob URL → interactive table.

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Agent unreachable at `install()` | Fallback to raw-bytes `display_data`. Debug log. API stays callable. |
| Agent unreachable mid-session | Retry open once; on failure, fallback for this call. Subsequent calls retry. |
| Put timeout (no ack) | `dx.put` raises `DxTimeoutError`. `dx.display` catches and falls back to raw-bytes. |
| Payload exceeds `MAX_BLOB_SIZE` | Agent replies `too_large`. `dx.put` raises `DxPayloadTooLargeError`. `dx.display` samples and retries, annotating the summary with `sampled=true`. |
| Missing serialization libs (no pyarrow, no fastparquet for pandas) | `install()` logs a warning at register time. `dx.display(df)` falls back to CSV-bytes or `repr`. |
| Unknown DataFrame type | Existing IPython formatter chain; dx does not interfere. |

Staging-area fallback (local spool, runtime agent drains async) is **not** in v1 — listed in Future.

## Testing

- **Unit (Python):** `dx._comm` multiplexing, timeout, fallback; `dx._summary` deterministic output for fixed fixtures; `dx._format` serializer priority chain with and without pyarrow/fastparquet.
- **Integration (Python + dev daemon):** start dev daemon, launch kernel, execute `dx.display(df)`; verify CRDT inline manifest has a ContentRef with expected hash, blob exists at that hash, `text/llm+plain` sibling entry present, no raw bytes on IOPub. Harness: `python/runtimed/tests/`.
- **CRDT filter test (Rust):** run a comm_open + comm_msg on `nteract.dx.blob`; assert `RuntimeStateDoc.comms` is empty after, blob exists, inline manifest composed correctly once the ref MIME arrives.
- **Agent unit (Rust):** `output_store::create_manifest` recognizes ref MIME, composes ContentRef without calling `BlobStore::put`.
- **End-to-end (WebdriverIO):** fixture notebook with `dx.display(df)`; assert sift/parquet renderer renders interactive table; assert IOPub capture contains no raw binary payload in the display_data bundle.
- **Vanilla-Jupyter smoke:** pytest that imports `dx`, calls `install()` with no runtime agent, calls `display(df)` — asserts fallback path emits a `display_data` with raw-bytes parquet and no exceptions.

## Dependencies (on other work)

- **Remote kernels (#1334)** — when the runtime agent runs on a separate host from the daemon, the runtime agent's current direct-filesystem-access to the blob store no longer works. A `PutBlob` frame on the runtime-agent↔daemon notebook-protocol socket (new request variant in `notebook-protocol/src/protocol.rs`) is needed. That frame belongs to the #1334 design, not this one, but dx's architecture is forward-compatible: the dx comm handler calls `BlobStore::put` today and would call whatever abstraction the remote-agent work lands tomorrow. No dx-side changes expected.

## Future: Interactive Query Backend

Buckaroo-class UX where the renderer sends a query (range, filter, sort, aggregation) back to the live kernel and receives a fresh Arrow batch — complementary to, not a replacement for, client-side paging via `sift-wasm`. Client-side paging stays the default; live queries engage when a DataFrame is too large to ship in full, or when the user wants server-side groupby / filter pushdown against the original data source.

The v1 design reserves the following hook points so this is additive, not disruptive:

- **Reserved comm target: `nteract.dx.query`.** The `nteract.dx.*` prefix filter already excludes it from CRDT persistence (see "Keeping the blob comm off the CRDT"). No new filter work needed.
- **Reserved `query` field in the blob-ref MIME.** v1 emits `null`; the future backend populates it with a handle_id and capability descriptor.
- **Handle lifecycle hook in `dx.display(df)`.** The serializer already owns the df reference — a live-query variant would also register the df in a kernel-side handle table keyed by a handle_id, returned in the ref MIME. When the handle is garbage collected or the kernel restarts, the renderer falls back to the static blob already stored. No new transport, no new renderer — just an optional upgrade.

Protocol sketch (not committed):

```
renderer → kernel (via frontend → comm relay → kernel):
  nteract.dx.query comm_msg:
    {op: "query", req_id, handle_id, query: { ... SQL-ish DSL ... }}

kernel → renderer:
  nteract.dx.query comm_msg:
    {op: "result", req_id, hash}   // result batch uploaded as a blob; renderer fetches
    (buffers carry raw bytes for small results, or hash-only for large)
```

Open questions left for that spec: query DSL (SQL? Ibis-style expression tree? Arrow Compute expressions?), handle eviction policy, multi-query concurrency, interaction with kernel interrupt. That design will pick one after surveying ADBC, DuckDB's DataFrame interfaces, and Ibis.

The important property for **this** spec: nothing in v1 paints us into a corner. Adding a live-query backend later is purely additive — a new comm target in the reserved namespace, a non-null value in an already-reserved ref-MIME field, and a handle table in `dx`.

## Future: Streaming

The comm protocol extends naturally to append-streaming Arrow batches (Arrow IPC streams, ADBC-style result handling):

- `op: "stream_open" { stream_id, content_type }` → runtime agent opens a streaming manifest in the CRDT (list of blob refs).
- `op: "put" { stream_id, req_id, ... }` appends a blob; runtime agent appends ContentRef to the stream's list.
- `op: "stream_close" { stream_id }` marks the stream complete.
- Renderer observes the list and appends rows as blobs arrive; shows a progress indicator until close.

Pre-work before designing this in detail: survey the current Arrow streaming landscape — Arrow IPC stream format, ADBC driver behavior (cursor-based batch delivery), how DuckDB/Polars/Snowflake ADBC drivers expose batch iteration, whether the natural unit is "Arrow RecordBatch" or "parquet row group." The right streaming abstraction depends on what producers natively emit.

## Future: dx.attach(path)

Convenience for uploading files from the kernel's filesystem (`dx.attach("/path/to/model.safetensors")`). Interacts with streaming (large files should chunk), with retention (attached files probably want an explicit pin), and with lifecycle (when does an attached blob get GC'd?). Worth its own design pass.

## Future: Lifecycle & Retention

Blobs written via dx are retained by the blob store's existing policy, keyed to CRDT ContentRef references. An orphan is created if `dx.put()` is called without a subsequent display or other reference. v1 accepts this. v2 may introduce explicit pinning or a notebook-scoped reference.

## Open Questions (v1.1+)

- Renderer UX for `summary` hints — banner vs. badge vs. inline header. Worth a visual design pass once the data is flowing.
- Do we want `dx.display(df, title=..., caption=...)` parameters exposed to the renderer via cell metadata or bundle metadata? Bundle metadata is simpler; cell metadata is more durable. Probably bundle metadata, in the ref MIME itself.
