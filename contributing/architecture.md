# Runtime Architecture Principles

This document defines the core architectural principles for the runtimed daemon and notebook system. These principles guide design decisions and help maintain consistency as the codebase evolves.

## Principles

### 1. Daemon as Source of Truth

The runtimed daemon owns all runtime state. Clients (UI, agents, CLI) are views into daemon state, not independent state holders.

**Implications:**
- Clients subscribe to daemon state via Automerge sync. The frontend maintains a local WASM doc for instant editing, but the daemon's doc is authoritative for execution and persistence
- State changes flow through the daemon, not peer-to-peer between clients
- If the daemon restarts, clients reconnect and resync

### 2. Automerge Document as Canonical Notebook State

The automerge document is the source of truth for notebook content: cells, their sources, metadata, and structure. All clients sync to this shared document.

**Implications:**
- Cell source code lives in the automerge doc
- To execute a cell: write it to the doc first, then request execution by cell_id
- Multiple clients editing the same notebook see each other's changes in real-time
- The daemon reads from the doc when executing, never from ad-hoc request parameters

### 3. On-Disk Notebook as Checkpoint

The `.ipynb` file on disk is a checkpoint/snapshot. The Automerge document is the live state.

**Implications:**
- Daemon reads `.ipynb` on first open, loads into automerge doc
- Daemon autosaves `.ipynb` on a debounce (2s quiet period, 10s max interval) via `spawn_autosave_debouncer` — no user action required
- Explicit save (Cmd+S) additionally runs cell formatting (ruff/deno fmt) before writing
- Unknown metadata keys in `.ipynb` are preserved through round-trips
- `NotebookAutosaved` broadcast clears the frontend dirty flag; `NotebookSaved` response confirms explicit saves

**Crash recovery:**
- Untitled notebooks (UUID-keyed rooms) persist their Automerge doc to `notebook-docs/{hash}.automerge` in the cache directory. On daemon restart, the room loads from this file.
- Saved notebooks reload from `.ipynb` (which autosave keeps current). Before deleting a persisted Automerge doc on reopen, the daemon snapshots it to `notebook-docs/snapshots/` (max 5 per notebook).
- Outputs are ephemeral. They live in the per-notebook RuntimeStateDoc and are not persisted.

**UUID-stable rooms:** Room keys are always UUIDs. When an untitled notebook is first saved, the daemon updates a secondary `path_index` map and broadcasts `PathChanged { path }` so peers can update local path tracking. The UUID never changes.

### 4. Local-First Editing, Synced Execution

Editing is local-first for responsiveness. Execution is always against synced state. The sync pipeline is incremental — changes propagate without full-document re-reads.

**Implications:**
- Type freely in cells; automerge handles sync and conflict resolution
- When you run a cell, you execute what's in the synced document
- No executing code that differs from the document state
- Source edits are debounced (20ms) before syncing to the daemon; `flushSync()` fires immediately before execute/save

**Incremental sync pipeline:**
- WASM `receive_frame()` computes a `CellChangeset` (in `notebook-doc/src/diff.rs`) by walking Automerge patches — O(delta), not O(doc)
- The changeset carries per-field flags (`source`, `outputs`, `execution_count`, `cell_type`, `metadata`, `position`, `resolved_assets`) per changed cell, plus lists of added/removed cell IDs
- `scheduleMaterialize` coalesces changesets within a 32ms window, then dispatches: structural changes → full materialization; output changes → per-cell cache-aware resolution (cache hits use `materializeCellFromWasm()`, cache misses resolve just that cell async); source/metadata-only → per-cell `materializeCellFromWasm()` via O(1) WASM accessors
- The split cell store (`notebook-cells.ts`) provides per-cell React subscriptions — `useCell(id)` re-renders only when that specific cell changes

**Per-cell accessors** (O(1) Automerge map lookups, available on `NotebookDoc`, `NotebookHandle`, and `DocHandle`):
- `get_cell_source(id)`, `get_cell_type(id)`, `get_cell_outputs(id)`, `get_cell_execution_count(id)`, `get_cell_metadata(id)`, `get_cell_position(id)`
- `get_cell_ids()` — position-sorted IDs (O(n log n) sort, reads only position strings, skips source/outputs/metadata)
- These are used by the frontend (per-cell materialization), the daemon (reading source for execution), and Python bindings (MCP tool responses)

### 5. Binary Separation via Manifests

Cell outputs are stored as content-addressed blobs with manifest references. This keeps large binary data (images, plots) out of the sync protocol.

**Implications:**
- Output broadcasts contain blob hashes, not inline data
- Clients resolve blobs from the blob store (disk or HTTP)
- Manifest format allows lazy loading and deduplication
- Large outputs don't block document sync

**On `.ipynb` save** the daemon still inlines most outputs as base64 — the same as vanilla Jupyter — so other tools can read the file. A small whitelist of nteract-specific MIMEs (currently just `application/vnd.apache.parquet`, the format the Sift dataframe viewer round-trips through) is externalized as a `BLOB_REF_MIME` entry pointing at the local blob store, keeping `.ipynb` size bounded for outputs that would otherwise serialize tens or hundreds of MiB. The Python `nteract/dx` package is the helper that produces those parquet payloads from a kernel. See `crates/runtimed/src/output_store.rs` (`should_externalize_mime_on_save`) and `docs/superpowers/specs/2026-04-14-ipynb-save-blob-refs-design.md`.

**Implementation details:**

The blob store uses content-addressed storage at `~/.cache/runt/blobs/`. Each blob is identified by its SHA-256 hash and stored in a two-level shard directory:

```
~/.cache/runt/blobs/
  a1/
    b2c3d4...       # raw bytes (actual PNG, UTF-8 text, etc.)
    b2c3d4....meta  # JSON metadata (media_type, size, created_at)
```

#### Text vs Binary Content — Critical Distinction

Jupyter kernels send binary data (images) as base64-encoded strings on the wire. The daemon **base64-decodes binary MIME types before storing** so the blob store holds actual binary bytes (real PNG, JPEG, etc.), not base64 text. This classification is determined by `is_binary_mime()`.

**Text MIME types** (`text/*`, `application/json`, `image/svg+xml`, anything `+json`/`+xml`):
- Stored as UTF-8 string bytes (or inlined in the manifest if ≤ 1KB)
- Resolved via `read_to_string()` / `response.text()`

**Binary MIME types** (`image/png`, `image/jpeg`, `audio/*`, `video/*`, most `application/*`):
- Base64-decoded by the daemon before storage — blob contains raw bytes
- **Always** stored as blobs (never inlined, regardless of size)
- Frontend resolves to `http://` blob URLs — browser fetches raw bytes directly via `<img src="...">`
- Python resolver reads raw bytes then base64-encodes for the `Output` struct
- Save-to-disk path reads raw bytes then base64-encodes for .ipynb format

**Important exception:** `image/svg+xml` is **TEXT**, not binary. Jupyter sends SVG as plain XML strings. The `+xml` suffix is the tell.

#### The `is_binary_mime` Contract

One canonical Rust implementation in `notebook-doc::mime` is the single source of truth. All Rust crates use this module — the old per-crate copies have been deleted. WASM owns MIME classification end-to-end and resolves `ContentRef`s to `Inline`/`Url`/`Blob` variants directly, so the frontend never has to classify MIMEs itself. The `looksLikeBinaryMime()` helper in `apps/notebook/src/lib/manifest-resolution.ts` is a thin cold-start safety net that runs before WASM is ready and is intentionally not the source of truth.

| Location | Function |
|----------|----------|
| `crates/notebook-doc/src/mime.rs` | `is_binary_mime()`, `mime_kind()`, `MimeKind` |

The rule:
- `image/*` → binary, **EXCEPT** `image/svg+xml` (plain XML text)
- `audio/*`, `video/*` → always binary
- `application/*` → binary by default, **EXCEPT**: `json`, `javascript`, `ecmascript`, `xml`, `xhtml+xml`, `mathml+xml`, `sql`, `graphql`, `x-latex`, `x-tex`, and anything ending in `+json` or `+xml`
- `text/*` → always text

#### Common Pitfalls

1. **"I'll store the base64 string directly"** — No. Binary MIME types must be base64-decoded before storing. Otherwise the blob server serves base64 text with `Content-Type: image/png` (wrong), and `<img src="blob-url">` breaks.
2. **"I'll use `read_to_string()` for all blobs"** — No. Binary blobs are raw bytes, not valid UTF-8. Check `is_binary_mime()` and use byte-mode reads, then base64-encode if the consumer needs a string.
3. **"SVG is an image, so it's binary"** — No. Jupyter sends SVG as plain XML text. The `+xml` suffix means text.
4. **"ContentRef needs a binary flag"** — It doesn't. The MIME type (the key in the manifest's `data` map) determines text vs binary. ContentRef is format-agnostic.

#### Data Flow

1. Kernel produces output → daemon's `output_prep.rs` converts to nbformat JSON
2. `output_store.rs` creates manifest:
   - Text MIME → `ContentRef::from_data()` (inline if ≤ 1KB, blob if larger)
   - Binary MIME → base64-decode → `ContentRef::from_binary()` (always blob)
3. Manifest is written as an inline Automerge Map in RuntimeStateDoc — MIME types and sizes are readable from the CRDT without blob fetch

Resolution varies by consumer:

| Consumer | Binary MIME | Text MIME |
|----------|------------|-----------|
| **Frontend** (WASM resolves `ContentRef` → `Inline`/`Url`/`Blob` variants) | Returns `http://` blob URL | Inline string or `response.text()` → string |
| **Python** (`output_resolver.rs`) | `fs::read()` → base64-encode | `read_to_string()` → string |
| **.ipynb save** (`output_store.rs`) | `resolve_binary_as_base64()` | `resolve()` → UTF-8 string |

Key files:
- `crates/notebook-doc/src/mime.rs` — Canonical MIME classification (`is_binary_mime`, `mime_kind`, `MimeKind`)
- `crates/runtimed/src/output_store.rs` — Manifest creation/resolution, `ContentRef`
- `crates/runtimed/src/blob_store.rs` — Content-addressed storage with atomic writes
- `crates/runtimed/src/blob_server.rs` — HTTP server (`GET /blob/{hash}`, serves raw bytes with correct `Content-Type`)
- `crates/runtimed-client/src/output_resolver.rs` — Shared Rust manifest resolution, Python/MCP consumers
- `apps/notebook/src/lib/manifest-resolution.ts` — Frontend resolution (WASM resolves `ContentRef` directly; `looksLikeBinaryMime()` is only the cold-start fallback)
- `apps/notebook/src/lib/materialize-cells.ts` — Assembles cells with resolved outputs

### 6. Process-Isolated Kernel Execution

Every kernel runs in a separate **runtime agent subprocess** (`runtimed runtime-agent`) that connects back to the daemon's Unix socket as an Automerge peer. The daemon coordinator handles environment resolution and spawns the runtime agent; the runtime agent owns the kernel process and writes outputs to RuntimeStateDoc.

```
┌────────────────────────────────────────────────────┐
│ Coordinator (runtimed daemon)                      │
│  Environment pools, trust, peer sync, persistence  │
│                                                    │
│  ExecuteCell → writes execution entry to CRDT      │
│  (source + seq number in RuntimeStateDoc)          │
└──────────┬─────────────────────────────────────────┘
           │ Unix socket (standard peer protocol)
           ▼
┌────────────────────────────────────────────────────┐
│ Runtime agent subprocess (runtimed runtime-agent)  │
│  Connects as Automerge peer, watches CRDT queue    │
│  Owns kernel process (ZMQ), IOPub, outputs         │
│  Writes results back via RuntimeStateDoc sync      │
└────────────────────────────────────────────────────┘
```

**CRDT-driven execution:** The coordinator writes execution entries (with source code and a monotonic sequence number) to RuntimeStateDoc. The runtime agent discovers new entries via Automerge sync, sorts by sequence number, and executes. No RPC needed for execution — it's pure state sync.

**RPC for lifecycle:** `LaunchKernel`, `InterruptExecution`, `ShutdownKernel`, `Complete`, `GetHistory`, and `SendComm` use `RuntimeAgentRequest`/`RuntimeAgentResponse` frames (0x01/0x02) over the socket.

**Implications:**
- Clients request kernel launch; they don't spawn kernels directly
- Environment selection is the daemon's decision based on notebook metadata
- Tool availability is the daemon's responsibility (bootstrap via rattler if needed)
- Clients are stateless with respect to runtime resources
- Each kernel is sandboxable at the OS level (process isolation enables future cgroup/seatbelt)
- Runtime agents can survive temporary disconnection and sync outputs on reconnect

### 7. Reuse Existing Protocols

New components should connect using existing protocols rather than inventing special transports. The runtime agent subprocess is a regular Unix socket peer — same handshake, same frame types, same Automerge sync as frontends and MCP servers. Execution state flows through the same RuntimeStateDoc CRDT that frontends already subscribe to.

**Implications:**
- New runtime backends (SSH remote, containerized) connect via the same socket protocol
- No special framing or serialization for internal components
- Debugging tools work uniformly (same frame types everywhere)
- The CRDT is the coordination mechanism — if state can be expressed as a CRDT mutation, prefer that over RPC

### Crate Boundaries

Three crates share "notebook" in the name but have distinct responsibilities:

| Crate | Owns | Consumers |
|-------|------|-----------|
| `notebook-doc` | Automerge document schema, cell CRUD, output writes, per-cell accessors, `CellChangeset` diffing, fractional indexing, presence encoding, frame type constants | daemon, WASM, Python bindings |
| `notebook-protocol` | Wire protocol types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`), connection handshake, frame parsing | daemon, `notebook-sync`, Python bindings |
| `notebook-sync` | Sync infrastructure (`DocHandle`), snapshot watch channel, per-cell accessors for Python clients, sync task management | Python bindings (`runtimed-py`) |

**Rule of thumb:** Document schema or cell operations → `notebook-doc`. New request/response/broadcast type → `notebook-protocol`. Python client sync behavior → `notebook-sync`.

The Tauri app crate (`crates/notebook/`) is glue — it wires Tauri commands to daemon requests and manages the socket relay. It does not own protocol types or document operations.

## Anti-Pattern: Bypassing the Document

The principle of "automerge as canonical state" is violated when execution requests include code directly instead of reading from the document.

**Correct flow:**
```
Client                              Daemon
  |                                   |
  |-- [WASM mutates local doc] -------|  // Instant, no round-trip
  |-- [sync frame 0x00] ------------>|  // invoke("send_frame")
  |<-- [sync frame 0x00] ------------|  // "notebook:frame" event
  |                                   |
  |-- ExecuteCell { cell_id } ------->|  // No code parameter
  |<-- CellQueued --------------------|
  |                                   |
  |<-- ExecutionStarted --------------|  // broadcast via notebook:frame
  |<-- Output -------------------------|  // broadcast via notebook:frame
  |<-- ExecutionDone -----------------|
```

**Incorrect flow (anti-pattern):**
```
Client                              Daemon
  |                                   |
  |-- QueueCell { cell_id, code } --->|  // Code passed directly!
  |<-- CellQueued --------------------|
  |                                   |
  // Other clients don't see the code
  // Document and execution are out of sync
```

## Testing Philosophy

- **E2E tests** (WebdriverIO): Slow but comprehensive, test full user journeys
- **Integration tests** (Python bindings): Fast daemon interaction tests via `runtimed-py`
- **Unit tests**: Pure logic, no I/O, fast feedback

Preference: Fast integration tests over slow E2E where possible. Use E2E for critical user journeys, integration tests for daemon behavior, unit tests for algorithms.

## Conformance Status

We are working toward full conformance with these principles.

| Principle | Status |
|-----------|--------|
| Daemon as source of truth | Conformant |
| Automerge as canonical state | Conformant |
| On-disk as checkpoint | Conformant |
| Local-first editing, synced execution | Conformant |
| Binary separation | Conformant |
| Daemon manages resources | Conformant |

The frontend now owns a local Automerge doc via `runtimed-wasm` WASM bindings, making it fully conformant with the canonical-state principle. Cell mutations (edits, reorders, deletes) are applied instantly in WASM with no RPC round-trip, satisfying local-first editing. `ExecuteCell` reads from the synced document.

## References

- `crates/notebook-protocol/src/protocol.rs` — Canonical wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `RuntimeAgentRequest`, `RuntimeAgentResponse`
- `crates/notebook-doc/src/lib.rs` — `NotebookDoc`: Automerge schema, cell CRUD, output writes, per-cell accessors
- `crates/notebook-doc/src/diff.rs` — `CellChangeset`: structural diff from Automerge patches
- `crates/notebook-doc/src/runtime_state.rs` — `RuntimeStateDoc`: kernel status, execution queue/lifecycle, env sync, comms
- `crates/notebook-sync/src/handle.rs` — `DocHandle`: sync infrastructure, per-cell accessors for Python clients
- `crates/runtimed/src/notebook_sync_server.rs` — `NotebookRoom`, room lifecycle, runtime agent sync handler, CRDT execution queue
- `crates/runtimed/src/runtime_agent.rs` — Runtime agent subprocess: Unix socket peer, CRDT queue watching, comm state diffing, kernel ownership
- `crates/runtimed/src/runtime_agent_handle.rs` — Coordinator-side runtime agent process management (spawn + monitor)
- `crates/runtimed/src/jupyter_kernel.rs` — `JupyterKernel`: kernel process spawn, ZMQ socket wiring, IOPub output routing
- `crates/runtimed/src/output_prep.rs` — Output-prep helpers: `QueueCommand`, `KernelStatus`, `QueuedCell`, iopub → nbformat conversion + display-update helpers, widget-buffer offload to the blob store
- `crates/runtimed/src/output_store.rs` — Output manifest creation/resolution, `ContentRef`
- `crates/notebook-doc/src/mime.rs` — Canonical MIME classification (`is_binary_mime`, `mime_kind`, `MimeKind`)
- `crates/notebook-sync/src/relay.rs` — `RelayHandle`: relay API for forwarding typed frames between WASM and daemon
- `crates/notebook-sync/src/connect.rs` — `connect_open_relay()`, `connect_create_relay()`: transparent byte pipe setup
- `crates/runtimed-wasm/src/lib.rs` — WASM bindings: local Automerge peer, frame demux, per-cell accessors, `CellChangeset`
- `crates/notebook/src/lib.rs` — Tauri commands and relay tasks (`send_frame` accepts raw binary via `tauri::ipc::Request`, `setup_sync_receivers`)
- `crates/notebook-doc/src/frame_types.rs` — Shared frame type constants (0x00–0x06)
- `apps/notebook/src/lib/frame-types.ts` — Frame type constants + `sendFrame()` binary IPC helper
- `apps/notebook/src/hooks/useAutomergeNotebook.ts` — WASM handle owner, `scheduleMaterialize`, `CellChangeset` dispatch
- `apps/notebook/src/lib/materialize-cells.ts` — `materializeCellFromWasm()` (per-cell) + `cellSnapshotsToNotebookCells()` (full)
- `apps/notebook/src/lib/notebook-cells.ts` — Split cell store: `useCell(id)`, `useCellIds()`, per-cell subscriptions
