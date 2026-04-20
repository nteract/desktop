---
paths:
  - crates/notebook-doc/**
  - crates/notebook-protocol/**
  - crates/notebook-sync/**
  - crates/runtimed/src/output_store*
  - crates/runtimed/src/blob_*
  - apps/notebook/src/lib/manifest-resolution*
---

# Runtime Architecture

## Core Principles

1. **Daemon as source of truth.** The runtimed daemon owns all runtime state. Clients are views, not independent state holders. If the daemon restarts, clients reconnect and resync.

2. **Automerge document as canonical notebook state.** Cell source, metadata, and structure live in the Automerge doc. To execute a cell, write it to the doc first, then request execution by cell_id. Never pass code as a request parameter.

3. **On-disk notebook as checkpoint.** The `.ipynb` file is a snapshot. The daemon autosaves on a debounce (2s quiet, 10s max). Explicit save (Cmd+S) also formats cells. Unknown metadata keys are preserved through round-trips.

4. **Local-first editing, synced execution.** Cell mutations happen instantly in the WASM Automerge peer. Execution always runs against the synced document. Source edits debounce at 20ms; `flushSync()` fires before execute/save.

5. **Binary separation via blob store.** Cell outputs use inline manifest Maps in the CRDT with `ContentRef` entries pointing to the blob store. MIME types and sizes are readable directly from the CRDT without any blob fetch. Large binary content (>1KB) goes to the content-addressed blob store; small content is inlined.

6. **Daemon manages runtime resources.** Clients request kernel launch; they never spawn kernels directly. Environment selection, tool availability, and lifecycle are the daemon's responsibility.

## Crate Boundaries

| Crate | Owns | Consumers |
|-------|------|-----------|
| `notebook-doc` | Automerge schema, cell CRUD, output writes, per-cell accessors, `CellChangeset` diffing, fractional indexing, presence encoding, frame type constants | daemon, WASM, Python bindings |
| `notebook-protocol` | Wire types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`), connection handshake, frame parsing | daemon, `notebook-sync`, Python bindings |
| `notebook-sync` | Sync infrastructure (`DocHandle`), snapshot watch channel, per-cell accessors for Python clients, sync task management | Python bindings (`runtimed-py`) |

**Rule of thumb:** Document schema or cell operations -> `notebook-doc`. New request/response/broadcast type -> `notebook-protocol`. Python client sync behavior -> `notebook-sync`.

The Tauri app crate (`crates/notebook/`) is glue -- it wires Tauri commands to daemon requests and manages the socket relay. It does not own protocol types or document operations.

## State Ownership

| State | Writer | Notes |
|-------|--------|-------|
| Cell source (`Text` CRDT) | Frontend WASM | Local-first, character-level merge |
| Cell position, type, metadata | Frontend WASM | User-initiated via UI |
| Notebook metadata (deps, runtime) | Frontend WASM | User edits deps, runtime picker |
| Cell outputs (inline manifests) | Daemon | Kernel IOPub -> blob store -> inline manifest Maps in RuntimeStateDoc |
| Execution count | Daemon | Set on `execute_input` from kernel |
| Widget state | Daemon (via `CommState`) | Kernel `comm_open`/`comm_msg` |
| RuntimeStateDoc (kernel status, queue, executions, env, trust) | Daemon | Separate per-notebook Automerge doc synced via frame `0x05` |

Reads are free for both sides. The daemon reads cell source for execution. The frontend reads outputs for rendering. Both use Automerge sync to stay current.

## RuntimeStateDoc

Each notebook room has a daemon-authoritative **RuntimeStateDoc** — a separate Automerge document (frame type `0x05`) that replaces state-carrying broadcasts. It tracks:

- **Kernel state**: status, starting phase (resolving → preparing_env → launching → connecting), name, language, env_source
- **Execution queue**: executing cell + execution_id, queued entries as `QueueEntry { cell_id, execution_id }`
- **Execution lifecycle**: per-execution_id map with status (`queued`/`running`/`done`/`error`), execution_count, success
- **Environment drift**: in_sync flag, added/removed packages
- **Trust state**: status and needs_approval flag

**The daemon is the sole writer.** Frontend reads via `useRuntimeState()`. Python reads via `notebook.runtime`.

Key files: `crates/notebook-doc/src/runtime_state.rs`, `apps/notebook/src/lib/runtime-state.ts`.

## Binary vs Text Content -- CRITICAL DISTINCTION

Jupyter kernels send binary data as base64-encoded strings on the wire. The daemon **base64-decodes binary MIME types before storing** so the blob store holds actual binary bytes.

**Text MIME types** (`text/*`, `application/json`, `image/svg+xml`, anything `+json`/`+xml`):
- Stored as UTF-8 string bytes (or inlined in manifest if < 1KB)
- Resolved via `read_to_string()` / `response.text()`

**Binary MIME types** (`image/png`, `image/jpeg`, `audio/*`, `video/*`, most `application/*`):
- Base64-decoded before storage -- blob contains raw bytes
- **Always** stored as blobs (never inlined, regardless of size)
- Frontend resolves to `http://` blob URLs
- Python resolver reads raw bytes -> `Output.data` is `bytes`

**Important exception:** `image/svg+xml` is TEXT, not binary. The `+xml` suffix is the tell.

### The `is_binary_mime` Contract

One canonical Rust implementation in `notebook-doc::mime` is the single source of truth for MIME classification. All Rust crates (`runtimed`, `runtimed-client`, `runtimed-wasm`) use this module — the old per-crate copies have been deleted. WASM now owns MIME classification end-to-end — it resolves `ContentRef`s to `Inline`/`Url`/`Blob` variants directly, so the frontend never needs to classify MIMEs itself.

| Location | Function |
|----------|----------|
| `crates/notebook-doc/src/mime.rs` | `is_binary_mime()`, `mime_kind()`, `MimeKind` |

Classification rules:
- `image/*` -> binary, **EXCEPT** `image/svg+xml`
- `audio/*`, `video/*` -> always binary
- `application/*` -> binary by default, **EXCEPT**: `json`, `javascript`, `ecmascript`, `xml`, `xhtml+xml`, `mathml+xml`, `sql`, `graphql`, `x-latex`, `x-tex`, and anything ending in `+json` or `+xml`
- `text/*` -> always text

### Common Pitfalls

1. **"Store the base64 string directly"** -- No. Binary MIME types must be base64-decoded before storing. Otherwise the blob server serves base64 text with `Content-Type: image/png` and `<img src="blob-url">` breaks.
2. **"Use `read_to_string()` for all blobs"** -- No. Binary blobs are raw bytes, not valid UTF-8. Check `is_binary_mime()` first.
3. **"SVG is an image, so it's binary"** -- No. Jupyter sends SVG as plain XML text. The `+xml` suffix means text.
4. **"ContentRef needs a binary flag"** -- It does not. The MIME type determines text vs binary. ContentRef is format-agnostic.

### Resolution by Consumer

| Consumer | Binary MIME | Text MIME |
|----------|------------|-----------|
| **Frontend** (WASM `ContentRef` resolution) | Resolves `ContentRef` to `Blob` variant -> `http://` blob URL | Resolves to `Inline` (string) or `Url` variant |
| **Python** (`output_resolver.rs`) | `fs::read()` -> raw `bytes` | `read_to_string()` -> string |
| **.ipynb save** (`output_store.rs`) | `resolve_binary_as_base64()` | `resolve()` -> UTF-8 string |

## Blob Store Details

Content-addressed storage at `~/.cache/runt/blobs/`, sharded by first 2 hex chars. Each blob has a `.meta` sidecar with `{media_type, size, created_at}`. Blobs are ephemeral -- derived from notebook content, regenerated from `.ipynb` on daemon restart.

**One-hop indirection:** Manifests are inline Automerge Maps in RuntimeStateDoc (not stored in the blob store). Each manifest contains `ContentRef` entries per MIME type: `{"inline": "<data>"}` for <=1KB text, `{"blob": "<hash>", "size": N}` for content >1KB or ANY binary. The CRDT points directly to content blobs — no manifest blob hop. MIME types and sizes are readable directly from the CRDT.

HTTP server at `127.0.0.1:<dynamic-port>` serves `GET /blob/{hash}` with correct `Content-Type` and `Cache-Control: immutable`.

## Incremental Sync Pipeline

1. **WASM `receive_frame()`** computes `CellChangeset` by walking `doc.diff(before, after)` patches. Cost is O(delta), not O(doc). Returns per-field flags per changed cell.
2. **`scheduleMaterialize`** coalesces within 32ms via `mergeChangesets()`. Structural changes -> full materialization. Output changes -> per-cell cache-aware resolution. Source/metadata only -> per-cell via O(1) WASM accessors.
3. **Split cell store** provides per-cell React subscriptions. `useCell(id)` re-renders only when that cell changes.
4. **Debounced outbound sync** batches keystrokes at 20ms. `flushSync()` fires before execute/save.

Per-cell WASM accessors (O(1) Automerge map lookups): `get_cell_source(id)`, `get_cell_type(id)`, `get_cell_outputs(id)`, `get_cell_execution_count(id)`, `get_cell_metadata(id)`, `get_cell_position(id)`, `get_cell_ids()`.

## Notebook Room Lifecycle

- **Autosave:** 2s quiet period, 10s max interval. `NotebookAutosaved` broadcast clears the frontend dirty flag.
- **UUID-stable rooms:** Room keys are always UUIDs. Saving an untitled notebook updates `path_index` and broadcasts `PathChanged { path }` to peers. The UUID never changes.
- **Crash recovery:** Untitled notebooks persist to `notebook-docs/{hash}.automerge`. Snapshots go to `notebook-docs/snapshots/`. `runt recover` exports to `.ipynb`.
- **Multi-window:** Multiple windows join the same room as separate Automerge peers.
- **Eviction:** After all peers disconnect, delayed eviction (default 30s via `keep_alive_secs`) shuts down the kernel and removes the room.

## Settings Sync

Settings (theme, default_runtime, etc.) sync via a **separate Automerge document** on the same Unix socket. Any window can write; all others receive changes. Frontend falls back to local `settings.json` if daemon is unavailable.

## Reserved Comm Namespace: `nteract.dx.*`

The `nteract.dx.*` target-name prefix is reserved for nteract's own kernel-side protocols (`nteract.dx.blob` for kernel → blob-store uploads; `nteract.dx.query` / `nteract.dx.stream` reserved for future use). Comms in this namespace are **filtered out of `RuntimeStateDoc::comms` by the runtime agent** — they never sync to the frontend as widget state or as `NotebookBroadcast::Comm` events, and their buffers go directly to the blob store. Do not register widget targets under this prefix. See `docs/superpowers/specs/2026-04-13-nteract-dx-design.md` for the protocol.

## Widget State (Current Architecture)

Widget state lives in **RuntimeStateDoc** (`doc.comms/` Automerge map):
- **Daemon:** Writes comm state from kernel IOPub (`comm_open`/`comm_msg(update)`/`comm_close`). State updates coalesce in a 16ms batch writer.
- **Frontend:** `WidgetStore` in `widget-store.ts` -- per-model subscriptions, IPY_MODEL_ reference resolution. Populated by a CRDT watcher that diffs `runtimeState.comms` and synthesizes Jupyter comm messages.
- **Frontend → Kernel:** State updates write to RuntimeStateDoc via CRDT writer. The runtime agent diffs comm state on each sync and forwards deltas to the kernel.

New clients receive widget state via normal RuntimeStateDoc CRDT sync (frame `0x05`). Custom widget messages (buttons, etc.) still use `NotebookBroadcast::Comm` as ephemeral events.

## Anti-Pattern: Bypassing the Document

Never pass code directly in execution requests. The correct flow: write to the CRDT, then send `ExecuteCell { cell_id }`. The daemon reads source from the synced document.

## Key Files

| File | Role |
|------|------|
| `crates/notebook-doc/src/lib.rs` | `NotebookDoc` -- Automerge schema, cell CRUD, output writes |
| `crates/notebook-doc/src/diff.rs` | `CellChangeset` -- structural diff from Automerge patches |
| `crates/notebook-doc/src/mime.rs` | Canonical MIME classification (`is_binary_mime`, `mime_kind`, `MimeKind`) |
| `crates/notebook-protocol/src/protocol.rs` | Wire types: requests, responses, broadcasts |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` -- sync infrastructure, per-cell accessors |
| `crates/runtimed/src/notebook_sync_server.rs` | `NotebookRoom`, room lifecycle, autosave, path_index |
| `crates/runtimed/src/output_prep.rs` | IOPub output-prep helpers (conversion, widget buffers, blob-store offload) |
| `crates/runtimed/src/output_store.rs` | Manifest creation/resolution, `ContentRef` |
| `crates/runtimed/src/blob_store.rs` | Content-addressed storage |
| `crates/runtimed/src/blob_server.rs` | HTTP server for blob retrieval |
| `crates/runtimed-client/src/output_resolver.rs` | Shared Rust resolution |
| `apps/notebook/src/lib/manifest-resolution.ts` | Frontend resolution (WASM resolves `ContentRef` directly) |
| `apps/notebook/src/lib/materialize-cells.ts` | WASM -> React conversion |
| `apps/notebook/src/lib/notebook-cells.ts` | Split cell store, per-cell subscriptions |