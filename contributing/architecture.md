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
- `runt recover` can list all snapshots and export any to `.ipynb`.

**Room re-keying:** When an untitled notebook is first saved to a file path, `rekey_ephemeral_room()` atomically changes the room key from a UUID to the canonical path, spawns a file watcher, cleans up the old persist file, and broadcasts `RoomRenamed` so all peers update their `notebook_id`.

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

**Implementation details:**

The blob store uses content-addressed storage at `~/.cache/runt/blobs/`. Each blob is identified by its SHA-256 hash and stored in a two-level shard directory:

```
~/.cache/runt/blobs/
  a1/
    b2c3d4...       # raw bytes
    b2c3d4....meta  # JSON metadata (media_type, size, created_at)
```

Data flow:
1. Kernel produces output → daemon's `output_store.rs` receives it
2. Large data (images, HTML) goes to `blob_store.rs` → returns hash
3. Daemon broadcasts `OutputManifest` with `ContentRef` (either `{ inline: "..." }` or `{ blob: "hash", size: N }`)
4. Frontend's `manifest-resolution.ts` checks `isManifestHash()` on output data
5. Blob refs are fetched from `blob_server.rs` via `http://127.0.0.1:{port}/blob/{hash}`
6. `materialize-cells.ts` converts resolved outputs to React-renderable `NotebookCell[]`

Key files:
- `crates/runtimed/src/blob_store.rs` — Content-addressed storage with atomic writes
- `crates/runtimed/src/blob_server.rs` — HTTP server for blob retrieval
- `crates/runtimed/src/output_store.rs` — Decides inline vs blob based on size threshold
- `apps/notebook/src/lib/manifest-resolution.ts` — `resolveManifest()`, `resolveContentRef()`
- `apps/notebook/src/lib/materialize-cells.ts` — Assembles cells with resolved outputs
- `apps/notebook/src/hooks/useManifestResolver.ts` — React hook for lazy resolution

### 6. Daemon Manages Runtime Resources

The daemon owns kernel lifecycle, environment pools, and tooling (ruff, deno, etc.).

**Implications:**
- Clients request kernel launch; they don't spawn kernels directly
- Environment selection is the daemon's decision based on notebook metadata
- Tool availability is the daemon's responsibility (bootstrap via rattler if needed)
- Clients are stateless with respect to runtime resources

### Crate Boundaries

Three crates share "notebook" in the name but have distinct responsibilities:

| Crate | Owns | Consumers |
|-------|------|-----------|
| `notebook-doc` | Automerge document schema, cell CRUD, output writes, per-cell accessors, `CellChangeset` diffing, fractional indexing, presence encoding, frame type constants | daemon, WASM, Python bindings |
| `notebook-protocol` | Wire protocol types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `CommSnapshot`), connection handshake, frame parsing | daemon, `notebook-sync`, Python bindings |
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

The frontend now owns a local Automerge doc via `runtimed-wasm` WASM bindings, making it fully conformant with the canonical-state principle. Cell mutations (edits, reorders, deletes) are applied instantly in WASM with no RPC round-trip, satisfying local-first editing. `ExecuteCell` reads from the synced document. The deprecated `QueueCell` (which accepts code as a parameter) is retained only for `runtimed-py` backwards compatibility.

## References

- `crates/notebook-protocol/src/protocol.rs` — Canonical wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `CommSnapshot`
- `crates/notebook-doc/src/lib.rs` — `NotebookDoc`: Automerge schema, cell CRUD, output writes, per-cell accessors
- `crates/notebook-doc/src/diff.rs` — `CellChangeset`: structural diff from Automerge patches
- `crates/notebook-sync/src/handle.rs` — `DocHandle`: sync infrastructure, per-cell accessors for Python clients
- `crates/runtimed/src/notebook_sync_server.rs` — `NotebookRoom`, room lifecycle, autosave debouncer, re-keying, sync loop
- `crates/runtimed/src/kernel_manager.rs` — Kernel process lifecycle, execution queue, IOPub output routing
- `crates/runtimed/src/comm_state.rs` — Widget comm state + Output widget capture routing
- `crates/runtimed/src/output_store.rs` — Output manifest creation, blob inlining threshold
- `crates/notebook-sync/src/relay.rs` — `RelayHandle`: relay API for forwarding typed frames between WASM and daemon
- `crates/notebook-sync/src/connect.rs` — `connect_open_relay()`, `connect_create_relay()`: transparent byte pipe setup
- `crates/runtimed-wasm/src/lib.rs` — WASM bindings: local Automerge peer, frame demux, per-cell accessors, `CellChangeset`
- `crates/notebook/src/lib.rs` — Tauri commands and relay tasks (`send_frame` accepts raw binary via `tauri::ipc::Request`, `setup_sync_receivers`)
- `crates/notebook-doc/src/frame_types.rs` — Shared frame type constants (0x00–0x04)
- `apps/notebook/src/lib/frame-types.ts` — Frame type constants + `sendFrame()` binary IPC helper
- `apps/notebook/src/hooks/useAutomergeNotebook.ts` — WASM handle owner, `scheduleMaterialize`, `CellChangeset` dispatch
- `apps/notebook/src/lib/materialize-cells.ts` — `materializeCellFromWasm()` (per-cell) + `cellSnapshotsToNotebookCells()` (full)
- `apps/notebook/src/lib/notebook-cells.ts` — Split cell store: `useCell(id)`, `useCellIds()`, per-cell subscriptions
