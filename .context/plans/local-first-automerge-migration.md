# Local-First Automerge Migration Plan

> Migrate the notebook from RPC-with-optimistic-UI to true local-first Automerge ownership.

## Status

| Phase | Status | Notes |
|-------|--------|-------|
| 0: Optimistic mutations | ✅ Done | [PR #542](https://github.com/nteract/desktop/pull/542) merged |
| 1.1–1.3: Eliminate `NotebookState` dual-write | ✅ Done | [PR #544](https://github.com/nteract/desktop/pull/544) merged |
| 1.4: Delegate save-to-disk to daemon | ✅ Done | [PR #545](https://github.com/nteract/desktop/pull/545) merged |
| 2: Frontend Automerge doc | ⬜ Not started | The real architectural shift — sub-PRs with feature flag |
| 3: Authority boundary hardening | ⬜ Not started | Formalize writer roles per field |
| 4: Optimize Tauri sync relay | ⬜ Not started | Binary IPC, reduce overhead |

---

## Problem

The current architecture maintains **four copies** of the notebook:

```
React useState ←→ invoke() ←→ NotebookState ←→ SyncClient AutoCommit ←→ Daemon AutoCommit
     (1)                          (2)                  (3)                     (4)
```

1. **React `cells` state** — `useState<NotebookCell[]>` in `useNotebook.ts:286`
2. **`NotebookState`** — `nbformat::v4::Notebook` struct in `crates/notebook/src/notebook_state.rs:152`
3. **Sync client's `AutoCommit`** — local Automerge replica in `NotebookSyncClient` (`crates/runtimed/src/notebook_sync_client.rs:433`)
4. **Daemon's `NotebookDoc`** — canonical Automerge doc in `NotebookRoom` (`crates/runtimed/src/notebook_sync_server.rs:452`)

Cell mutations like `addCell` and `deleteCell` use blocking RPC — the frontend `await`s `invoke()`, which round-trips through copies (2), (3), and (4) before the frontend updates copy (1). The user pays full IPC + sync latency for operations that don't need backend involvement.

### What's already right

- **Outputs flow through Automerge, not RPC.** Frontend `onOutput` is intentionally `() => {}` (`App.tsx:271-276`). Outputs arrive via `notebook:updated` Automerge sync.
- **Daemon reads source from Automerge for execution.** `ExecuteCell` reads from the doc (`notebook_sync_server.rs:1850`), not from whatever the frontend sent.
- **Source uses Automerge Text CRDT.** Character-level merging via `update_text()` (Myers diff).
- **Python agents are full Automerge peers.** The `runtimed` Python package (`crates/runtimed-py`) connects via the same Unix socket, holds its own `AutoCommit` doc, and syncs bidirectionally. Agents can `create_cell`, `set_source`, `append_source`, `delete_cell` — all as CRDT operations that merge with concurrent user edits.

### What's wrong

- **`NotebookState` is redundant.** It shadows the sync client's Automerge doc. The receiver loop at `lib.rs:420-424` overwrites it on every peer update.
- **Frontend has no Automerge doc.** It receives materialized `CellSnapshot[]` and can't do local CRDT mutations.
- **`addCell` and `deleteCell` block on RPC** for things the frontend can determine locally (UUID generation, "last cell" validation).
- **Full-state replacement on sync.** `notebook:updated` does `setCells(newCells)`, clobbering any in-flight optimistic state, which forces the blocking RPC pattern.

### Peers to account for

| Peer | Connection | Automerge doc? | Writes |
|------|-----------|----------------|--------|
| Frontend (React) | Tauri IPC | ❌ No — receives `CellSnapshot[]` | Cells, source, metadata via `invoke()` |
| Tauri process | In-process | ✅ Via `NotebookSyncClient` | Relays frontend writes |
| Daemon (`runtimed`) | Unix socket server | ✅ Canonical `NotebookDoc` | Outputs, execution_count, kernel status |
| Python agent (`runtimed` package) | Unix socket client | ✅ Own `AutoCommit` | Cells, source (create/edit/delete) |
| Additional windows | Unix socket client | ✅ Via `NotebookSyncClient` | Same as frontend |

Python agents are first-class peers. An agent calling `session.append_source(cell_id, text)` while a user types in the same cell produces concurrent Text CRDT operations that Automerge merges automatically. The migration must preserve this — agents should never need to change their API.

---

## Target architecture

```
┌──────────────────────────────┐
│  Frontend                    │
│  Local AutoCommit doc        │
│  React state derived from doc│
│  All cell CRUD is local      │
└──────────┬───────────────────┘
           │ Binary Automerge sync messages
           │ (Tauri events or custom protocol)
┌──────────▼───────────────────┐
│  Tauri Process (thin relay)  │
│  Forwards sync messages      │
│  Handles: save, format, OS   │
└──────────┬───────────────────┘
           │ Unix socket (v2 typed frames)
┌──────────▼───────────────────┐
│  Daemon                      │
│  Canonical AutoCommit doc    │
│  Writes: outputs, exec_count │
│  Kernel, env management      │
└──────────┬───────────────────┘
           │ Unix socket (v2 typed frames)
┌──────────▼───────────────────┐
│  Python Agent                │
│  Own AutoCommit doc          │
│  Writes: cells, source       │
│  Reads: outputs, status      │
└──────────────────────────────┘
```

All peers hold Automerge docs. Mutations are local-first. Sync is automatic and bidirectional.

---

<details>
<summary><h2>Phase 0: Make existing mutations optimistic ✅</h2></summary>

**Goal:** Eliminate user-visible latency for `addCell` and `deleteCell` without touching the sync layer.

**Effort:** Small (days). **Risk:** Low — `notebook:updated` full-replace acts as convergence.

### 0.1 — Optimistic `deleteCell`

Current (`useNotebook.ts:477-485`):
```ts
const deleteCell = useCallback(async (cellId: string) => {
  try {
    await invoke("delete_cell", { cellId });
    setCells((prev) => prev.filter((c) => c.id !== cellId));
    setDirty(true);
  } catch (e) { ... }
}, []);
```

Change to:
```ts
const deleteCell = useCallback((cellId: string) => {
  // Validate locally — don't delete the last cell
  setCells((prev) => {
    if (prev.length <= 1) return prev;
    return prev.filter((c) => c.id !== cellId);
  });
  setDirty(true);
  // Fire-and-forget sync to backend
  invoke("delete_cell", { cellId }).catch((e) =>
    logger.error("[notebook] delete_cell sync failed:", e)
  );
}, []);
```

- [x] Move "last cell" check to frontend
- [x] `setCells()` runs before `invoke()`
- [x] `invoke()` is fire-and-forget (catch-only)
- [x] Backend `delete_cell` command becomes infallible from frontend's perspective

### 0.2 — Optimistic `addCell`

Current (`useNotebook.ts:451-475`):
```ts
const addCell = useCallback(async (cellType, afterCellId?) => {
  const newCell = await invoke<NotebookCell>("add_cell", { cellType, afterCellId });
  setCells((prev) => { /* insert newCell */ });
  ...
}, []);
```

Change to:
```ts
const addCell = useCallback((cellType, afterCellId?) => {
  const cellId = crypto.randomUUID();
  const newCell: NotebookCell = {
    id: cellId,
    cell_type: cellType,
    source: "",
    outputs: [],
    execution_count: null,
  };
  setCells((prev) => { /* insert newCell at position */ });
  setFocusedCellId(cellId);
  setDirty(true);
  // Fire-and-forget — backend uses provided cellId
  invoke("add_cell", { cellId, cellType, afterCellId }).catch((e) =>
    logger.error("[notebook] add_cell sync failed:", e)
  );
  return newCell;
}, []);
```

- [x] Generate UUID on frontend via `crypto.randomUUID()`
- [x] Construct `NotebookCell` locally
- [x] `setCells()` runs before `invoke()`
- [x] Update `add_cell` Tauri command signature to accept `cell_id: String` parameter
- [x] Backend uses provided ID instead of generating one

### 0.3 — Verify convergence

- [x] Confirm `notebook:updated` event still reconciles any divergence
- [x] Test: add cell while daemon is disconnected → cell persists after reconnect
- [x] Test: delete cell while daemon is disconnected → deletion syncs on reconnect
- [x] Test: two windows delete the same cell concurrently → both converge

</details>

---

<details>
<summary><h2>Phase 1.1–1.3: Eliminate <code>NotebookState</code> dual-write ✅</h2></summary>

**Goal:** Remove the redundant `nbformat::Notebook` struct from the Tauri process as a dual-write target. The `NotebookSyncHandle` becomes the single source of truth for all cell and metadata operations.

**Effort:** Medium (1-2 weeks). **Risk:** Medium — many Tauri commands reference `NotebookState`.

### What was done

All ~25 call sites in `crates/notebook/src/lib.rs` that dual-wrote to both `NotebookState` and the sync handle were migrated:

- **Cell mutations** (`update_cell_source`, `add_cell`, `delete_cell`) — sync handle only, no more `NotebookState` write.
- **Cell reads** (`load_notebook`) — reads from `handle.get_cells()`, falls back to `NotebookState` when daemon disconnected.
- **Path reads** (`has_notebook_path`, `get_notebook_path`, `detect_pyproject`, `detect_pixi_toml`, `detect_environment_yml`, `detect_deno_config`) — read from new `context.path` field.
- **All 16 metadata commands** — read/write via sync handle `get_metadata`/`set_metadata`. Read commands fall back to `NotebookState` when disconnected.
- **Format cell** — reads source from sync handle, writes formatted result back.
- **Receiver loop sync-back removed** — the block that overwrote `NotebookState` cells/metadata from Automerge on every peer update is gone.
- **`WindowNotebookContext`** gained `path: Arc<Mutex<Option<PathBuf>>>` and `working_dir: Option<PathBuf>`. `notebook_state` retained for save (Phase 1.4).

### Additional fixes discovered during QA

- **Trust signature round-trip** — `approve_notebook_trust`/`verify_notebook_trust` now use raw JSON read/write (`get_raw_metadata_additional`, `set_raw_trust_in_metadata`) to preserve `trust_signature`/`trust_timestamp` which aren't modeled in the typed `RuntMetadata` struct.
- **Runtime detection** — `get_runtime_from_sync` now matches `NotebookState::get_runtime()` semantics: `ks.language == "python"` check and `language_info` fallback added.
- **Initial metadata in handshake** — New `initial_metadata` field on `Handshake::NotebookSync` sends the kernelspec with the connection handshake so the daemon has it before auto-launching. Fixes Deno notebooks getting Python kernels on File → New Notebook As → Deno.
- **`save_notebook_as` stale cells** — Refreshes `NotebookState` from Automerge before serializing (same pattern as `save_notebook`'s local fallback).
- **`push_metadata_to_sync` removed** — Was clobbering dependency changes by pushing stale `NotebookState` metadata to the sync handle on save.
- **`add_cell` with disconnected daemon** — Now returns `Err("Not connected to daemon")` instead of a ghost cell.

### Remaining `NotebookState` usage (Phase 1.4 scope)

| Consumer | Why it still uses `NotebookState` |
|----------|----------------------------------|
| `save_notebook` | Serializes to `.ipynb` (refreshes from Automerge first) |
| `save_notebook_as` | Same, plus updates path |
| `clone_notebook_to_path` | Clones notebook struct |
| `initialize_notebook_sync` | First-peer population reads cells from `NotebookState` (loaded from disk) |
| `reconnect_to_daemon` | Passes `NotebookState` to `initialize_notebook_sync` |
| Disconnected fallbacks | ~8 metadata read commands fall back to `NotebookState` when daemon is down |

- [x] Route all cell reads through `handle.get_cells()` instead of `state.lock()`
- [x] Route all cell writes through sync handle commands
- [x] Route all metadata reads/writes through sync handle
- [x] Remove receiver loop sync-back
- [x] Add `path` and `working_dir` to `WindowNotebookContext`

</details>

---

<details>
<summary><h2>Phase 1.4: Delegate save-to-disk to daemon ✅</h2></summary>

**Goal:** Move notebook save-to-disk from the Tauri process to the daemon, eliminating the last major `NotebookState` consumer.

### What was done

- `SaveNotebook` request now accepts `path: Option<String>` for save-as support and returns `NotebookSaved { path: String }` with the daemon-normalized absolute path (`.ipynb` appended if needed).
- `save_notebook_to_disk()` refactored to accept optional target path. Relative paths are rejected (daemon CWD is unpredictable as a launchd service).
- **Format-on-save moved to daemon.** New `format_notebook_cells()` function in `notebook_sync_server.rs` runs ruff (Python) or deno fmt (Deno) on all code cells, writes formatted source back to Automerge, and broadcasts changes to all peers. Client-side formatting loop removed from both `save_notebook` and `save_notebook_as`.
- Local `NotebookState::serialize()` fallback removed. Daemon save is now required — disconnected daemon returns a clear error.
- `save_notebook_as` uses the daemon-returned path as canonical for window title, `NotebookState.path`, `context.path`, and room reconnection.
- Python bindings: `session.save(path=None)` and `await async_session.save(path=None)` added to both `Session` and `AsyncSession`.

### Known issue: save-as loses outputs

When saving an untitled notebook via Save As, outputs from the current session are lost in the saved file. This happens because save-as creates a new room (new notebook_id derived from the new path), and the daemon's `save_notebook_to_disk` merges with the existing file (which doesn't exist yet for a new path). The outputs exist in the old room's Automerge doc but not the new one. This may be acceptable since save-as triggers a new kernel anyway, but worth evaluating.

### Remaining `NotebookState` usage

`NotebookState` is no longer used for persistence. Remaining 13 call sites are:
- **Initial loading** — `create_window_context`, `initialize_notebook_sync` (first-peer population from disk)
- **Path/dirty tracking** — `save_notebook` (path check), `save_notebook_as` (path + dirty update)
- **Clone** — `clone_notebook_to_path` (local serialization for "Make a Copy")
- **Reconnect** — `reconnect_to_daemon` (passes state to re-initialize sync)
- **Disconnected fallbacks** — 8 metadata read commands fall back to `NotebookState` when daemon is down

Full removal of the `NotebookState` struct is deferred — it still serves as the disconnected-daemon fallback and the initial notebook loading path. These will naturally go away as Phase 2 gives the frontend its own Automerge doc.

- [x] Add `NotebookRequest::SaveToDisk` variant to the protocol
- [x] Handle in `handle_notebook_request`: call existing `save_notebook_to_disk`, return success/failure
- [x] Frontend's save command sends request to daemon via sync handle
- [x] Move format-on-save (ruff/deno fmt) to the daemon save path
- [x] For `save_notebook_as`: frontend handles file dialog, sends new path to daemon, daemon writes, Tauri uses daemon-returned path
- [x] Remove `NotebookState` serialization from `crates/notebook/src/lib.rs`
- [x] Python bindings for `save(path=None)`

</details>

---

## Phase 2: Frontend Automerge doc

**Goal:** The frontend owns a local Automerge document. All document mutations happen instantly on the local doc. React state is derived from it. The Tauri process becomes a sync relay.

**Effort:** Large (2-4 weeks). **Risk:** Medium — WASM bundle size (~200KB gzip), performance on large notebooks needs profiling.

**Strategy:** Feature flag toggle with sub-PRs. Build `useAutomergeNotebook` alongside `useNotebook`, controlled by a feature flag (debug menu setting or env var). Both exist in the codebase during development. Each sub-PR is independently mergeable and testable. When the Automerge path is stable, flip the default and delete the old path.

### Sub-PR 2A — WASM + schema setup

Add `@automerge/automerge` to the frontend build pipeline. Zero runtime behavior change.

- [ ] `npm install @automerge/automerge`
- [ ] Add Vite WASM plugins: `vite-plugin-wasm`, `vite-plugin-top-level-await`
- [ ] Verify WASM initialization works in dev and production builds
- [ ] Define TypeScript document schema in `apps/notebook/src/automerge/schema.ts`:

```ts
// Document schema — matches Rust NotebookDoc (notebook_doc.rs:9-23)
interface NotebookDocSchema {
  notebook_id: string;
  cells: Array<{
    id: string;
    cell_type: string;       // "code" | "markdown" | "raw"
    source: string;           // Automerge Text CRDT
    execution_count: string;  // "5" or "null"
    outputs: string[];        // JSON-encoded outputs or manifest hashes
  }>;
  metadata: {
    runtime?: string;
    notebook_metadata?: string;  // JSON-encoded NotebookMetadataSnapshot
  };
}
```

- [ ] Add feature flag infrastructure (setting or env var for `USE_AUTOMERGE_NOTEBOOK`)
- [ ] Measure WASM bundle size impact (target: <250KB gzip)

### Sub-PR 2B — Sync relay infrastructure

Tauri event plumbing for binary Automerge sync messages. The old `useNotebook` path still runs — this just adds the new pipes.

| Event | Direction | Payload |
|-------|-----------|---------|
| `automerge:to-daemon` | Frontend → Tauri → Daemon | Binary (base64 initially, `Channel<Vec<u8>>` if needed) |
| `automerge:from-daemon` | Daemon → Tauri → Frontend | Binary (base64 initially, `Channel<Vec<u8>>` if needed) |

- [ ] Add Tauri commands/events that forward binary Automerge sync messages to/from the daemon Unix socket
- [ ] Keep `NotebookSyncHandle.send_request()` for kernel commands (execute, interrupt, save, etc.)
- [ ] Define initialization protocol: Tauri connects to daemon, performs initial sync exchange, sends doc bytes to frontend via one-shot event
- [ ] Handle "first peer populates room" vs "joining existing room" cases
- [ ] Ensure Python agents already connected to the room see the frontend's initial state

### Sub-PR 2C — Hook replacement + migration (behind feature flag)

The core architectural change. `useAutomergeNotebook` replaces `useNotebook` when the feature flag is enabled.

- [ ] Create `apps/notebook/src/hooks/useAutomergeNotebook.ts`
- [ ] Frontend owns a local Automerge doc — all cell mutations are local `Automerge.change()` calls:
  - `addCell` — `crypto.randomUUID()`, insert into Automerge list
  - `deleteCell` — remove from Automerge list (last-cell guard local)
  - `updateCellSource` — `Automerge.updateText()` (Myers diff internally)
  - Cell reorder — Automerge list move (new capability)
- [ ] `materializeCells()` converts Automerge doc → `NotebookCell[]` for React rendering
- [ ] Handle output manifest hash resolution (blob store HTTP fetch) — outputs arrive via Automerge sync from daemon
- [ ] Frontend generates sync messages after each `Automerge.change()` via `Automerge.generateSyncMessage()`
- [ ] Frontend applies incoming sync messages via `Automerge.receiveSyncMessage()`
- [ ] Wire into `App.tsx` behind the feature flag toggle
- [ ] Remove `invoke()` calls for cell mutations (add, delete, source edit) — keep for: execute, interrupt, shutdown, launch kernel, format cell, save
- [ ] Replace `notebook:updated` event handling with Automerge sync message handling

Key implementation details:
- `Automerge.change()` returns a new immutable doc — compatible with React's `useState`
- `Automerge.updateText()` does Myers diff on strings internally — same as the Rust `update_text`
- `Automerge.splice()` for direct character-level edits (CodeMirror integration, future)
- For cell source editing, `Automerge.updateText(doc, path, newValue)` computes minimal edits

### Sub-PR 2D — Cleanup (after feature flag is flipped)

- [ ] Remove `useNotebook.ts`
- [ ] Remove feature flag infrastructure
- [ ] Remove `NotebookSyncClient` local Automerge doc from Tauri process (reduce to relay-only)
- [ ] Remove remaining `NotebookState` disconnected-daemon fallbacks (frontend Automerge doc is the fallback)
- [ ] Delete `notebook_state.rs` struct and unused methods

### Performance validation (ongoing across sub-PRs)

- [ ] Profile `Automerge.change()` + `materializeCells()` latency on 100-cell notebook
- [ ] Profile on 500-cell notebook
- [ ] Profile on 1000-cell notebook with large outputs
- [ ] If needed: implement selective re-materialization (only re-read changed cells)

---

## Phase 3: Authority boundary hardening

**Goal:** Formalize which fields each peer can write, so CRDT convergence matches user expectations.

**Effort:** Small (days). **Risk:** Low.

### Writer roles

| Field | Writer(s) | Rationale |
|-------|----------|-----------|
| `cells` list (add/delete/reorder) | Frontend, Python agents | User/agent intent, instant feedback |
| `cells[i].source` (Text CRDT) | Frontend, Python agents | User types, agent streams code |
| `cells[i].cell_type` | Frontend, Python agents | Toggle code↔markdown |
| `cells[i].outputs` | **Daemon only** | Kernel produces these |
| `cells[i].execution_count` | **Daemon only** | Kernel assigns these |
| `metadata.notebook_metadata` | Frontend, Python agents | Dependency management |
| `metadata.runtime` | Frontend | User selects runtime |

### Conflict scenarios

| Scenario | Resolution |
|----------|-----------|
| Two frontends add cell at same position | Automerge list CRDT — both cells appear, deterministic order |
| User and agent edit same cell source concurrently | Automerge Text CRDT — character-level merge |
| Two windows delete same cell | Automerge — double-delete is idempotent |
| Agent appends source while user edits beginning | Text CRDT merges cleanly — append is position-independent |
| Daemon writes outputs while frontend edits source | Different fields — no conflict |

### `clear_outputs` ownership

Today the frontend clears outputs locally (`clearCellOutputs` in `useNotebook.ts:441`) AND the daemon clears via Automerge. In the new model:

- [ ] Daemon owns output clearing — it clears when execution starts
- [ ] Frontend stops rendering stale outputs when it sees `execution_started` broadcast
- [ ] Remove frontend-side `clearCellOutputs` mutation on the Automerge doc
- [ ] Frontend can show a visual indicator (dimming) while waiting for new outputs

---

## Phase 4: Optimize Tauri sync relay (optional, future)

**Goal:** Make the Tauri relay as thin and fast as possible. Tauri stays in the path — the frontend should never speak directly to the Unix socket for security reasons.

**Effort:** Small. **Risk:** Low.

### Why Tauri stays in the loop

The daemon Unix socket is unauthenticated — any process that can reach it can read/write any notebook. In production this is fine because only local processes connect. But letting the webview open a raw socket (even localhost WebSocket) would let a compromised renderer talk to the daemon without Tauri's mediation. Tauri should own the connection lifecycle: auth, handshake, connection teardown on window close.

### Binary IPC precedent

We already pass binary data through Tauri events for widget buffers (`useDaemonKernel.ts:316-321`) — `number[][]` round-tripped via JSON. This works but is not ideal for Automerge sync messages which are opaque binary blobs that don't benefit from JSON encoding.

### Optimization options

| Approach | Tradeoff |
|----------|----------|
| **Tauri `Channel<Vec<u8>>`** | Tauri v2 channels support streaming binary data. Use a Rust-side channel to push sync messages to the frontend as raw bytes, and a command to send bytes back. Avoids JSON base64/array encoding overhead. |
| **Tauri custom protocol** | Register an `automerge://` protocol handler. Frontend `fetch()`es sync messages as binary responses. Good for large payloads but adds HTTP framing overhead for small, frequent sync messages. |
| **Base64 in events** | Simplest. Encode sync messages as base64 strings in Tauri events. ~33% overhead but Automerge sync messages are typically small (sub-KB for incremental changes). Good enough to start. |

Recommendation: start with base64-in-events in Phase 2 (simplest, unblocks everything), then migrate to `Channel<Vec<u8>>` if profiling shows encoding overhead matters.

The blob store already has its own HTTP server (`http://127.0.0.1:{blobPort}/blob/{hash}`) for large output payloads, so sync messages only carry manifest hashes, not raw output data. This keeps sync message sizes small.

- [ ] Benchmark sync message sizes in practice (expect sub-KB for source edits, larger for cell add with initial content)
- [ ] If base64 overhead is measurable: migrate to Tauri `Channel<Vec<u8>>` for binary streaming
- [ ] Profile end-to-end latency: frontend Automerge change → Tauri relay → daemon apply → broadcast → other peer receives

---

## Key files reference

### Frontend
| File | Role |
|------|------|
| `apps/notebook/src/hooks/useNotebook.ts` | Current notebook state hook (to be replaced) |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Daemon kernel execution, broadcasts |
| `apps/notebook/src/App.tsx` | Top-level component, wires hooks together |

### Tauri process (crates/notebook)
| File | Role |
|------|------|
| `crates/notebook/src/lib.rs` | Tauri commands, sync initialization |
| `crates/notebook/src/notebook_state.rs` | `NotebookState` — the struct to eliminate |

### Daemon (crates/runtimed)
| File | Role |
|------|------|
| `crates/runtimed/src/notebook_doc.rs` | `NotebookDoc` — Automerge doc wrapper, schema |
| `crates/runtimed/src/notebook_sync_client.rs` | `NotebookSyncClient` — local Automerge peer |
| `crates/runtimed/src/notebook_sync_server.rs` | `NotebookRoom`, daemon-side sync loop |
| `crates/runtimed/src/connection.rs` | v2 typed frame protocol |

### Python agent
| File | Role |
|------|------|
| `crates/runtimed-py/src/session.rs` | `Session` — Python API, full Automerge peer |
| `crates/runtimed-py/src/async_session.rs` | `AsyncSession` — async variant |
| `python/runtimed/src/runtimed/_mcp_server.py` | MCP server — AI agent tools |

---

## Non-goals

- **Frontend direct access to the daemon socket.** The daemon's Unix socket is unauthenticated — any process that connects can read/write any notebook room. Tauri must mediate all daemon communication so the webview renderer never holds a raw socket. This is a security boundary, not a performance optimization to remove later.
- **Replacing the daemon's Automerge with `automerge-repo`.** The daemon's custom sync protocol (v2 typed frames multiplexing sync + requests + broadcasts) is well-suited to the single-doc-per-room model. `automerge-repo` is designed for multi-doc repos with discovery — unnecessary complexity here.
- **Moving Automerge to the Python agent.** The Python agent already has a full Automerge peer via the Rust `NotebookSyncClient`. No JS Automerge needed on the Python side.
- **Real-time collaborative cursors.** Desirable but separate concern. Can be implemented later via Automerge ephemeral messages or the existing broadcast channel.
- **Operational transform for CodeMirror.** The `@automerge/codemirror` plugin exists but is a Phase 2+ consideration. Initial implementation can use `updateText` on every change event.