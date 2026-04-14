# nteract/dx — Python → Blob Store for the Modern nteract

**Status:** Implemented
**Date:** 2026-04-13 (rev. 2026-04-14)
**Related:** #1334 (SSH remote runtimes), #1307 (kernel sandboxing), #1759 (parquet summaries, sift-wasm), #1762 (initial PR)

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

`nteract/dx` fills the missing piece: a Python library that lets a kernel hand bytes to the blob store **without** putting them on IOPub as base64 JSON, plus a clean display surface on top.

## Goals

1. Eliminate the "raw bytes in IOPub JSON" pattern for parquet, Arrow, images, and arbitrary binary payloads.
2. Ride the existing runtime-agent-in-the-loop architecture so remote kernels benefit without new transport.
3. Ship a user-facing API (`dx.install`, `dx.display`) that feels as natural as `IPython.display`.
4. Degrade gracefully in vanilla Jupyter and plain `python` — no superpowers, no exceptions.
5. Forward-compatible with the planned ZMQ-less, in-process runtime agent.
6. Let Python compute `text/llm+plain` summaries at the source (typed columns, cheap iteration), rather than re-deriving them server-side.

## Non-Goals (v1)

- Live kernel-side interactive query backend (Buckaroo-style round-trip). Deferred — see **Future: Interactive Query Backend**. This is the primary use of the reserved comm namespace.
- Append-streaming Arrow batches. See **Future: Streaming**.
- `dx.attach(path)` convenience for large file uploads. See Future.
- Authenticated HTTP write endpoint on the blob server.
- A blob-write RPC on the runtime-agent↔daemon socket. Not needed for v1 (runtime agent has filesystem access to the blob store). Documented under Dependencies as a prerequisite for remote kernels.
- Automatic formatter registration on `import`. Explicit `dx.install()` only.
- A kernel-side ack / reply path. v1 is fire-and-forget (see Architecture).

## Architecture

### The bytes ride IOPub `buffers`, not a comm

Jupyter's messaging protocol already supports binary frames at the **envelope level**, not the JSON content: every `JupyterMessage` can carry trailing ZMQ `buffers` alongside the 4 standard JSON parts (header / parent_header / metadata / content). ipywidgets uses this today for binary widget state (`buffer_paths` pointing into `state` + trailing frames).

v1 dx uploads use the same mechanism for `display_data`:

```
┌──────────────────────────────────┐      ┌──────────────────────────┐
│  Python kernel process           │      │  runtime agent           │
│                                  │      │                          │
│  import dx                       │      │  IOPub listener          │
│  dx.install()                    │      │                          │
│                                  │      │  on display_data:        │
│  dx.display(df):                 │      │    preflight_ref_buffers │
│    1. parquet = serialize(df)    │      │      → BlobStore::put    │
│    2. h = sha256(parquet)        │      │        for each          │
│    3. summary = summarize(df)    │      │        BLOB_REF_MIME +   │
│    4. kernel.session.send(       │──▶   │        buffer_index      │
│         iopub_socket,            │ZMQ   │    create_manifest       │
│         "display_data",          │IOPub │      recognises ref MIME │
│         content={                │      │      → ContentRef::      │
│           BLOB_REF_MIME: {       │      │          from_hash       │
│             hash: h,             │      │    (ref MIME is NOT      │
│             content_type: …,     │      │     emitted as a         │
│             size, buffer_index:0,│      │     manifest entry)      │
│             summary:{…}          │      │                          │
│           },                     │      │  fork/merge inline       │
│           "text/llm+plain": …    │      │    manifest (existing    │
│         },                       │      │    path, unchanged)      │
│         buffers=[parquet]        │      │                          │
│       )                          │      │                          │
└──────────────────────────────────┘      └──────────────────────────┘
                                                     │
                                                     ▼
                                          ┌──────────────────────────┐
                                          │  CRDT inline manifest    │
                                          │  ContentRef::Blob{hash,  │
                                          │   size} under target_ct  │
                                          └──────────────────────────┘
                                                     │
                                                     ▼
                                          ┌──────────────────────────┐
                                          │  Frontend WASM resolves  │
                                          │  ContentRef → blob URL   │
                                          │  at render time          │
                                          └──────────────────────────┘
```

One IOPub message, one round-trip through the blob store. No ack, no comm, no deadlock.

### Observability: ref MIME is a transport detail, not output

`BLOB_REF_MIME` appears on the wire (IOPub) but **never** in the inline output manifest. The agent's `create_manifest` ref-MIME branch recognizes the ref, composes a `ContentRef::Blob { blob: hash, size }` under the wrapped `content_type` (e.g. `application/vnd.apache.parquet`), and drops the ref MIME itself. Consequences:

- The CRDT's inline manifest for a cell that did `dx.display(df)` looks identical to one that received a raw-bytes `display_data` — both show `application/vnd.apache.parquet` → `ContentRef::Blob`.
- The frontend resolves `ContentRef::Blob` to `http://<host>:<port>/blob/<hash>`, so the rendered output shows a blob URL under the target content_type whether the upload was new-path or old-path.
- To distinguish paths, observe the IOPub wire: new path sends `BLOB_REF_MIME` + trailing ZMQ buffers; old path sends `application/vnd.apache.parquet` with base64 bytes inside the JSON content. Agent logs `[dx] blob-ref` warnings only on failures (hash mismatch, missing content_type, out-of-range buffer_index). A successful dx display is silent in agent logs by design.

### Why fire-and-forget

An earlier draft used a dedicated Jupyter comm (`nteract.dx.blob`) for the upload, with an ack back to the kernel carrying the content hash. That design hit a classic deadlock: ipykernel dispatches shell `comm_msg`s on the **same asyncio loop** that runs cells, so `dx.put()` blocking on a `threading.Event.wait()` for the ack prevented the ack from ever being dispatched. Kernel-opened comms cannot synchronously round-trip during cell execution.

Moving the upload to IOPub `buffers` sidesteps the loop entirely:

- The hash is content-addressed — Python and agent independently arrive at the same SHA-256. No round-trip needed to agree.
- Python never blocks. `kernel.session.send` is a fast write to the local ZMQ socket.
- The agent processes IOPub messages sequentially, so the buffer write completes before `create_manifest` runs for the same message.
- Failure modes are local: if the buffer is missing or the hash is malformed, the agent logs a warn and drops the ref entry. Vanilla Jupyter users get a fallback (see "Vanilla Jupyter" below) but it should be noted that this means they don't get enjoyment of the new display path.

### Reserved comm namespace: `nteract.dx.*`

Even though v1 doesn't use a comm for uploads, the `nteract.dx.*` target-name prefix stays reserved for future bidirectional dx subsystems (see Future sections). The runtime agent filters any comm on this prefix out of `RuntimeStateDoc::comms` and out of `NotebookBroadcast::Comm`, logging a `warn!` with the raw target name so a kernel opening a reserved target we haven't implemented yet is visible in logs rather than silently leaking into widget state.

Filter implementation (`crates/runtimed/src/dx_blob_comm.rs` + IOPub task in `crates/runtimed/src/jupyter_kernel.rs`):

- `DX_NAMESPACE_PREFIX = "nteract.dx."`
- `is_dx_target(target)` matches the prefix (rejects `"nteract.dx"` and `"nteract.dx."` to keep the namespace root reservable).
- `DxTarget::Unknown(String)` — v1 has no handler variants. When a future subsystem lands, we add a named variant (e.g. `Query`, `Stream`) and a live-dispatch branch in the IOPub CommMsg arm.

### Durability: hash-primary, URL-ephemeral

The blob server port is dynamic. We do not try to stabilize it.

- The ref MIME carries **hash + content_type (+ optional summary hints + buffer_index)**. No URL.
- The CRDT stores a `ContentRef::Blob { blob: hash, size }` — same shape used today for inline binary outputs.
- Frontend WASM derives the current blob-server URL at render time.

## Protocol

### Blob-ref MIME

```
MIME: application/vnd.nteract.blob-ref+json
Body:
{
  "hash": "sha256-hex",
  "content_type": "application/vnd.apache.parquet",
  "size": 104857600,
  "buffer_index": 0,
  "summary": {
    "total_rows": 148820,
    "included_rows": 10000,
    "sampled": true,
    "sample_strategy": "head"
  },
  "query": null
}
```

- `hash`: SHA-256 hex of the trailing buffer bytes. Agent re-hashes on receipt for sanity; a mismatch logs a warn and drops the entry.
- `content_type`: the real MIME type the frontend should render (e.g. `application/vnd.apache.parquet`, `image/png`). The ref MIME itself is never emitted as a manifest entry — `create_manifest` resolves it and composes a ContentRef under this `content_type`.
- `buffer_index`: which trailing ZMQ buffer frame this ref points at (0 in v1, reserved for multi-blob displays in the future).
- `summary`: optional renderer hints — lets sift show "showing 10,000 of 148,820 rows (head sample)" without opening the parquet.
- `query`: reserved `null` for v1. Populated later with a handle_id + capabilities when Interactive Query Backend ships.

### Kernel → runtime-agent: `display_data` with buffers

```python
kernel.session.send(
    kernel.iopub_socket,
    "display_data",
    content={
        "data": {
            BLOB_REF_MIME: { hash, content_type, size, buffer_index: 0, summary: {...}, query: None },
            "text/llm+plain": "...",
        },
        "metadata": {},
        "transient": {},
    },
    parent=kernel.get_parent("shell"),
    ident=kernel.topic("display_data"),
    buffers=[parquet_bytes],
)
```

- Mirrors how ipykernel's own `publish_display_data` calls `Session.send` — `ident` is the IOPub topic (bytes), `parent` is the parent header dict.
- No new transport: runtimelib already serializes trailing `buffers: Vec<Bytes>` as ZMQ frames on send (`RawMessage::from_jupyter_message`), and the receive side parses them back (`into_jupyter_message`). ipywidgets exercises this path today.

### Agent side: buffer preflight + ref MIME resolution

`crates/runtimed/src/output_store.rs`:

- **`preflight_ref_buffers(nbformat, buffers, blob_store)`** — walks the display_data `data` bundle; for each `BLOB_REF_MIME` entry with a `buffer_index`, writes `buffers[idx]` to `BlobStore::put` (content-addressed — idempotent, dedupes by hash). Sanity-checks computed hash vs declared hash; logs a warn on mismatch.
- **`create_manifest` ref-MIME branch** (pre-existing): when walking the bundle, sees `BLOB_REF_MIME`, composes `ContentRef::from_hash(hash, size)` under `content_type`, does **not** emit the ref MIME itself as a manifest entry. If the blob is missing (preflight failed or not attempted), the ref is dropped with a warn.
- **`ContentRef::from_hash(hash, size)`** — trivial wrapper over the existing `ContentRef::Blob` variant for the case where the blob is already stored.

`crates/runtimed/src/jupyter_kernel.rs` IOPub task: calls `preflight_ref_buffers` before `create_manifest` in both the Output-widget capture path (`display_data` routed to a widget) and the normal cell-output path.

## API Surface (v1)

```python
import dx
dx.install()   # called from the notebook bootstrap or explicitly in cell 1

# Bare df on last cell line routes through dx.
df

# Explicit call same result.
dx.display(df)
```

Everything else (`dx.put`, `dx.attach`, `dx.display_blob_ref`) is deferred — those operations lack a natural display target to attach to and are better designed alongside streaming / attach / query work.

### Public API

- `dx.install() -> None` — registers IPython formatters for `pandas.DataFrame` and `polars.DataFrame` (if installed). Idempotent. Safe to call in vanilla Jupyter or plain Python.
- `dx.display(obj) -> None` — upgraded display that routes DataFrames through the blob-store path; for other types, hands off to `IPython.display.display`.
- `dx.BlobRef(hash: str, size: int)` — content-addressed reference (dataclass). Internal to dx's protocol; exposed for potential future public use.
- `dx.BLOB_REF_MIME` — the MIME constant.
- `dx.DxError` — base exception.

### Display pipeline: two-stage `mimebundle_formatter` + `display_pub` hook

`dx.install()` wires into two documented IPython / ipykernel extension
points:

1. **`ip.display_formatter.mimebundle_formatter.for_type(pd.DataFrame, fn)`** —
   the formatter serializes the DataFrame to parquet, hashes locally,
   stashes the bytes in a thread-local buffer map keyed by the hash,
   and returns a mimebundle with `application/vnd.nteract.blob-ref+json`
   + Python-side `text/llm+plain`. IPython's `DisplayFormatter.format`
   then merges that bundle with the default pandas formatters, so the
   final publish carries our ref MIME **plus** fallback `text/html` /
   `text/plain` for hosts that don't understand the ref MIME.

2. **`ip.display_pub.register_hook(hook)`** (on `ipykernel.zmqshell.ZMQDisplayPublisher`, documented public API) — the hook runs on
   every outgoing `display_data` and `update_display_data` message
   right before `session.send` would emit it. When the hook sees our
   ref MIME in the message, it pops the stashed parquet bytes by hash
   and calls `session.send(pub_socket, msg, ident=topic, buffers=[parquet])`
   directly, returning `None` to suppress the default (buffer-less)
   send. For messages that don't carry the ref MIME, the hook returns
   the message unchanged and `session.send` fires normally.

**Why this shape.** An earlier draft used `ipython_display_formatter`
returning `True` to claim exclusive display, and published via
`session.send` inside the formatter. That blocked the default
pandas HTML/plain fallback (which broke display in vanilla IPython)
and had no access to `display_id` / `update=True`, which broke
`h.update(df)` on a display handle — the `update_display_data` message
was never emitted and the handle orphaned. Hook-based attachment fixes
both: the fallback chain runs, so vanilla IPython still renders, and
the hook fires on `update_display_data` with the `display_id` already
populated in `msg.content.transient`, so updates Just Work with the
buffer path attached.

### Behavior that `dx.install()` changes globally

`dx.install()` is an explicit opt-in that mutates kernel-wide display
behavior:

1. **DataFrame display carries the nteract ref MIME + parquet buffer.**
   Hosts that understand the ref MIME (nteract runtime agent) resolve
   to a blob URL and render via the sift parquet renderer. Hosts that
   don't understand it (vanilla JupyterLab, plain IPython) fall back
   to the text/html output in the same bundle. No rendering is lost.

2. **Altair and plotly default renderers get flipped to `"nteract"`.**
   Plotly's `nteract` renderer emits only
   `application/vnd.plotly.v1+json`, dropping the terminal / browser
   fallback. In plain-IPython sessions this means plotly figures stop
   rendering outside nteract. (The nteract frontend has a plotly
   renderer plugin that handles this MIME correctly.)

This is an acceptable tradeoff for the intended use case — kernels
managed by the nteract runtime agent always have matching consumers.
For DataFrame display the HTML fallback keeps vanilla hosts working;
for altair/plotly the third-party switch is the bigger opt-in.

### Update-display semantics

`h = display(df, display_id=True); h.update(df2)` works natively
through the hook. IPython's `update_display_data` path runs unchanged:
the formatter returns a bundle (including the ref MIME), the buffer
bytes are stashed, the display publisher emits an
`update_display_data` message with `transient={"display_id": X}`, and
the hook attaches buffers to that message just like it does for
initial displays. The frontend receives the update with parquet
buffers attached and updates the existing output in place.

### Display ownership

`dx.install()` registers on IPython's `ipython_display_formatter` (not `mimebundle_formatter`) and the registered callback returns `True` when it publishes a display. This tells IPython's display chain to skip every other formatter for the object — bare `df` on the last cell line emits exactly one `display_data`, not our upgrade *plus* pandas' default HTML/plain.

In vanilla IPython / plain Python, the formatter returns `None` instead — IPython's default chain (HTML, plain, etc.) runs unchanged.

### Serialization: graceful degradation

DataFrame → parquet goes through a best-available-encoder chain in `dx/_format.py`:

1. `pandas.DataFrame` → `pyarrow.Table.from_pandas(df)` + `pyarrow.parquet.write_table`. Preferred.
2. `polars.DataFrame` → `df.write_parquet(buf)` (native).
3. If the full payload exceeds a per-message ceiling (default 90 MiB, under the agent's 100 MiB `MAX_BLOB_SIZE`), the serializer downsamples via `df.head(n)` with a binary-search-ish loop. The resulting parquet carries fewer rows, and the ref MIME's `summary.sampled` flag is set to `true` so the renderer can show "showing N of M rows."

### `text/llm+plain` generation (Python-side)

`dx/_summary.py` produces the summary from the DataFrame directly — shape, per-column dtype + null count, small head sample. If the serialized parquet was downsampled, the header explicitly calls that out: `"DataFrame (pandas): 10,000 rows × 12 columns (sampled from 148,820 total rows)"`. `repr-llm`'s server-side synthesis remains the fallback for legacy paths that don't use dx.

## Components

### `python/dx/` (uv workspace member)

- `dx/__init__.py` — public API (`install`, `display`, `BlobRef`, `BLOB_REF_MIME`, `DxError`).
- `dx/_env.py` — environment detection (`PLAIN_PYTHON` / `IPYTHON_NO_KERNEL` / `IPYKERNEL`).
- `dx/_summary.py` — `text/llm+plain` generator.
- `dx/_format.py` — DataFrame → parquet serializer with downsampling.
- `dx/_refs.py` — `BlobRef`, `BLOB_REF_MIME`, `build_ref_bundle`.
- `dx/_format_install.py` — IPython formatter registration + `kernel.session.send` with buffers.

Added as workspace member in repo-root `pyproject.toml` alongside `runtimed`, `nteract`, `gremlin`.

### Agent (Rust)

- `crates/notebook-doc/src/mime.rs` — `BLOB_REF_MIME` constant (text JSON classification).
- `crates/runtimed/src/dx_blob_comm.rs` — reserved `nteract.dx.*` namespace filter (`is_dx_target`, `classify_dx_target`, `DxTarget::Unknown(String)`). No live handlers in v1.
- `crates/runtimed/src/output_store.rs` — `preflight_ref_buffers`, `ContentRef::from_hash`, ref-MIME branch in `convert_data_bundle`.
- `crates/runtimed/src/jupyter_kernel.rs` — IOPub DisplayData/ExecuteResult handlers call `preflight_ref_buffers` before `create_manifest`; CommOpen/CommMsg/CommClose filter the dx namespace out of `RuntimeStateDoc::comms`.

### Frontend / renderer

**No changes required for v1.** ContentRef resolution and the sift/parquet renderer already handle hash-based refs end-to-end. Summary-hint rendering ("showing N of M rows") is a separate frontend task — protocol carries the hints, UI comes later.

## Data Flow: `dx.display(df)`

1. User executes `df` (last line) or `dx.display(df)`.
2. `dx._format.serialize_dataframe(df)` → `(parquet_bytes, "application/vnd.apache.parquet")`, with a sampling decision based on size.
3. `dx._summary.summarize_dataframe(df, ...)` → `text/llm+plain` string.
4. `hashlib.sha256(parquet_bytes).hexdigest()` → hash.
5. `kernel.session.send("display_data", content={data: {BLOB_REF_MIME: {...}, "text/llm+plain": ...}, ...}, buffers=[parquet_bytes], parent=..., ident=kernel.topic("display_data"))`.
6. Python formatter returns `True` → IPython skips all other formatters for `df`.
7. Runtime agent's IOPub task receives `display_data` with `buffers=[parquet_bytes]`:
   - `preflight_ref_buffers` writes `buffers[0]` to `BlobStore::put` under `application/vnd.apache.parquet`, verifies hash.
   - `create_manifest` walks the data bundle, sees `BLOB_REF_MIME`, composes `ContentRef::Blob { blob: hash, size }` under `application/vnd.apache.parquet`. Does not emit the ref MIME as a manifest entry. Keeps `text/llm+plain` as an inline manifest entry.
8. fork/merge writes the inline manifest into the cell's outputs in the CRDT.
9. CRDT sync → frontend → WASM resolves `ContentRef` to `http://<host>:<port>/blob/<hash>` → sift/parquet renderer loads the blob → interactive table.

## Error Handling

| Condition | Behavior |
|-----------|----------|
| No ipykernel (plain IPython / plain Python) | Formatter returns `None`. IPython's default chain runs unchanged. |
| `kernel.session.send` raises | Debug log; formatter returns `None` → IPython falls back to default display. |
| Serialization fails (missing pyarrow) | Debug log; formatter returns `None` → IPython falls back to `repr`. |
| Payload exceeds `MAX_BLOB_SIZE` | Serializer downsamples automatically; `summary.sampled=true` in the ref MIME. |
| Ref MIME body missing `hash` / `content_type` | Agent logs warn, skips preflight and drops the ref entry. Other bundle entries (e.g. `text/llm+plain`) still render. |
| `buffer_index` out of range | Agent logs warn, skips preflight. Ref will fail `BlobStore::exists` during `create_manifest` and be dropped. |
| Hash mismatch (computed vs declared) | Agent logs warn. The blob is stored at the computed hash; the ref's declared hash will fail `BlobStore::exists` in `create_manifest` and the entry is dropped. |
| Blob store write fails | Agent logs warn. Ref entry dropped. |
| Future kernel sends comm_open on a reserved `nteract.dx.*` target with no v1 handler | Agent filters the comm out of `RuntimeStateDoc::comms`; logs a warn carrying the raw target name. |

## Testing

- **Python unit:** formatter registers on `ipython_display_formatter`; under ipykernel fires `session.send` with `buffers=[parquet]` and returns `True`; outside ipykernel returns `None`; display-formatter attribute-access regression guard (non-callable `display_formatter` fake).
- **Rust unit:** `preflight_ref_buffers` writes blob when buffer present; no-op when no buffers; `create_manifest` recognizes `BLOB_REF_MIME` and composes `ContentRef::Blob`; missing-blob ref drops with warn; `is_dx_target` / `classify_dx_target` prefix semantics.
- **Manual smoke (performed against dev daemon):** bare `df` (3-row and 50k-row) → exactly one `display_data` with `application/vnd.apache.parquet` resolved to a blob URL + `text/llm+plain` summary. No default HTML/plain duplicate. Blob hash in the URL matches the SHA-256 computed Python-side.
- **Planned:** Python integration test (`python/runtimed/tests/test_dx_integration.py`), WebdriverIO E2E with the sift parquet renderer. See follow-up tasks.

## Dependencies (on other work)

- **Remote kernels (#1334)** — when the runtime agent runs on a separate host from the daemon, the current direct-filesystem `BlobStore::put` from the agent no longer works. A `PutBlob` frame on the agent↔daemon notebook-protocol socket is needed there. Not a dx change — the dx path already goes through `BlobStore::put`, which the remote work would re-target at a daemon RPC.

## Future: Interactive Query Backend

Buckaroo-class UX where the renderer sends a query (range, filter, sort, aggregation) back to the live kernel and receives a fresh Arrow batch. Complementary to client-side paging via `sift-wasm`: client-side stays the default, live queries engage for large datasets or server-side groupby/filter.

This is the primary motivator for keeping the comm namespace reserved. The round-trip happens **while the kernel is idle** (between cell executions) or on the control thread — neither subject to the v1 deadlock that drove the buffer-based upload design.

Reserved hooks ready for this design pass:

- `nteract.dx.query` comm target — already filtered out of `RuntimeStateDoc::comms`.
- `query` field in the ref MIME — currently `null`; future backend populates it with `handle_id` + capability descriptor.
- Handle lifecycle hook in `dx.display(df)` — serializer already owns the df reference; a live-query variant would also register the df in a kernel-side handle table keyed by `handle_id`.

Open questions for that spec: query DSL (SQL? Ibis expression tree? Arrow Compute?), handle eviction, multi-query concurrency, interaction with kernel interrupt, ADBC / DuckDB / Ibis survey.

## Future: Streaming

Append-streaming Arrow batches for live data. Builds on the same reserved comm namespace:

- `nteract.dx.stream` target.
- `op: stream_open { stream_id, content_type }` → agent opens a streaming manifest in the CRDT.
- `op: put { stream_id, req_id, ... }` appends each batch as a new blob; agent appends its `ContentRef` to the stream's manifest.
- `op: stream_close { stream_id }` marks the stream complete.
- Renderer observes the manifest and appends rows as blobs arrive; shows progress until close.

Pre-work: survey the Arrow IPC stream format, ADBC cursor-based delivery, and how DuckDB/Polars/Snowflake ADBC drivers expose batch iteration. The right abstraction depends on what producers natively emit (Arrow `RecordBatch` vs parquet row group vs raw bytes).

## Future: `dx.attach(path)`

Convenience for uploading files from the kernel filesystem (`dx.attach("/path/to/model.safetensors")`). Interacts with streaming (chunked uploads for large files), retention (attached files probably want an explicit pin), and lifecycle (when does an attached blob get GC'd?). Worth its own design pass.

## Future: Lifecycle & Retention

v1 ties blob retention to the CRDT's ContentRef references — a blob with no references is eligible for GC. `dx.display(df)` always produces a reference (the inline manifest), so there are no orphans in v1. `dx.attach` and other future APIs that detach upload from display will need an explicit pin or a notebook-scoped reference.

## Open Questions (v1.1+)

- Renderer UX for `summary` hints — banner vs. badge vs. inline header. Worth a visual design pass.
- Do we want `dx.display(df, title=..., caption=...)` parameters via bundle metadata or cell metadata? Bundle metadata is simpler; cell metadata is more durable. Probably bundle metadata, embedded in the ref MIME body.

## Implementation trail

- #1762 — initial PR (spec + plan + python/dx scaffold + comm-based upload path).
- 626766f0 — first runtime-agent landing (comm handler + ref MIME in `create_manifest`).
- 72a027ab — Codex review fixes (display_formatter attribute access, `nteract.dx.*` prefix-wide filter, empty-buffer guard).
- 983ad417 — this revision: swap comm-ack upload for `display_data` + `buffers`; collapse Python `_comm` module; simplify `DxTarget`; retain reserved namespace for future interactive features.
