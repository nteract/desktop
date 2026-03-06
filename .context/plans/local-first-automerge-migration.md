# Local-First Automerge Migration Plan

> Migrate the notebook from RPC-with-optimistic-UI to true local-first Automerge ownership.

## Status

| Phase | Status | Notes |
|-------|--------|-------|
| 0: Optimistic mutations | ✅ Done | [PR #542](https://github.com/nteract/desktop/pull/542) merged |
| 1.1–1.3: Eliminate `NotebookState` dual-write | ✅ Done | [PR #544](https://github.com/nteract/desktop/pull/544) merged |
| 1.4: Delegate save-to-disk to daemon | ✅ Done | [PR #545](https://github.com/nteract/desktop/pull/545) merged |
| 2: Frontend Automerge doc | 🚧 Blocked | [Draft PR #547](https://github.com/nteract/desktop/pull/547) — phantom cell bug in sync relay, exploratory spikes planned |
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

## Phase 2: Frontend Automerge doc — [Draft PR #547](https://github.com/nteract/desktop/pull/547)

**Goal:** The frontend owns a local Automerge document. All document mutations happen instantly on the local doc. React state is derived from it. The Tauri process becomes a sync relay.

**Effort:** Large (2-4 weeks). **Risk:** Medium — WASM bundle size (~200KB gzip), performance on large notebooks needs profiling.

**Strategy:** Feature flag toggle with sub-PRs. Build `useAutomergeNotebook` alongside `useNotebook`, controlled by a feature flag (`localStorage` or `?automerge=true` URL param). Both exist in the codebase during development. Each sub-PR is independently mergeable and testable. When the Automerge path is stable, flip the default and delete the old path.

### Hard-won lessons (from QA and debugging)

| Lesson | Detail |
|--------|--------|
| **Must use `@automerge/automerge@^2.2.x`** | The Rust daemon uses `automerge = "0.7"` (crates.io). The JS `@automerge/automerge` v2.2.x bundles WASM from a compatible automerge-rs version. v3.x may have wire-format divergence — we hit "Cell not found in document" errors when the frontend sent v3 sync messages to the Rust 0.7 relay. Stick with v2 until the Rust crate bumps. |
| **Use `import { next as Automerge }`** | In v2, `updateText` and `splice` live on the `next` sub-export, not the top-level. `import { next as Automerge } from "@automerge/automerge"` gives the full API. |
| **List ops use proxy methods** | In v2 `next`, list mutations inside `Automerge.change()` use `(d.cells as any).insertAt(idx, item)` and `(d.cells as any).deleteAt(idx)` — NOT top-level `Automerge.insertAt()` / `Automerge.deleteAt()` which don't exist in v2. |
| **Scalar strings return as `RawString`** | v2 `next` wraps scalar strings (non-Text) in `RawString` objects. Use `String(value)` for comparison, not `===`. Text CRDT fields (like `source`) return as plain strings. |
| **Do NOT `syncToBackend()` after `Automerge.load()`** | Sending a sync message from a freshly-loaded doc with a fresh `initSyncState()` triggers a full re-sync that can corrupt the daemon's state. Let the daemon initiate sync — the bidirectional exchange starts naturally when daemon changes arrive via `automerge:from-daemon`. |
| **Doc bytes bootstrap ≠ sync handshake** | Loading doc bytes via `get_automerge_doc_bytes` + `Automerge.load()` gives the frontend the right data but does NOT establish a sync relationship. The Tauri relay's `frontend_peer_state` has never exchanged messages with the frontend peer, so when the frontend generates sync messages after local mutations (e.g., `addCell`), the relay can't apply them correctly. The frontend's cells never reach the daemon. |
| **Frontend peer state must not exist before GetDocBytes** | If `frontend_peer_state` is initialized at task startup (`sync::State::new()`), daemon sync acks during cell population cause the relay to generate sync messages for a peer that doesn't exist yet. Those stale messages buffer in `raw_sync_tx`. When the frontend later loads doc bytes and receives them, the CRDT merge produces phantom cells. Fix: start `frontend_peer_state` as `None`, only init inside `GetDocBytes`. |
| **Phantom cell bug (UNRESOLVED)** | Even with all the above fixes, the frontend still produces phantom cells from daemon sync responses. The Tauri relay receives frontend sync messages, decodes them OK, but the relay's doc NEVER gains the frontend's cells (BEFORE/AFTER always identical). Meanwhile, daemon sync responses to the frontend produce cells with IDs that don't exist in any Rust-side doc. The relay architecture (Tauri as intermediary with its own Automerge doc + separate peer states for daemon and frontend) may be fundamentally flawed — the three-way sync (frontend ↔ Tauri ↔ daemon) with `doc.save()` bootstrap may not be achievable with the Automerge sync protocol. |
| **Sync needs multiple roundtrips** | The Automerge sync protocol is not one-shot. `generateSyncMessage` / `receiveSyncMessage` must be called in a loop until both sides return `null` messages. The compat test validates this. |
| **Compat test is essential** | `apps/notebook/src/__tests__/automerge-compat.test.ts` validates Rust 0.7 ↔ JS v2 interop: load fixture bytes, sync roundtrip, change+sync. Run this after any Automerge version change on either side. Fixture bytes exported from `crates/runtimed/src/notebook_doc.rs` test. |
| **Unit tests pass but runtime fails** | The JS compat test (load, sync roundtrip, change+sync) passes. But the actual runtime sync through the Tauri relay produces phantom cells. The relay's intermediary role — receiving from both daemon and frontend, maintaining two sync states, forwarding changes — introduces complexity that unit tests don't cover. |

### Sub-PR 2A — WASM + schema setup ✅

Add `@automerge/automerge` to the frontend build pipeline. Zero runtime behavior change.

- [x] `pnpm add @automerge/automerge@^2.2.9` (NOT v3 — see lessons above)
- [x] Add Vite WASM plugins: `vite-plugin-wasm`, `vite-plugin-top-level-await`
- [x] Configure vitest in `apps/notebook/vite.config.ts` for compat tests
- [x] Verify WASM initialization works in dev and production builds
- [x] Define TypeScript document schema in `apps/notebook/src/lib/automerge-schema.ts`
- [x] Add feature flag: `localStorage` + `?automerge=true` URL param (`apps/notebook/src/lib/feature-flags.ts`)
- [x] Add Rust fixture export test (`crates/runtimed/src/notebook_doc.rs`)
- [x] Add JS compat test (`apps/notebook/src/__tests__/automerge-compat.test.ts`) — 3 tests: load, sync roundtrip, change+sync
- [ ] Measure WASM bundle size impact (target: <250KB gzip)

### Sub-PR 2B — Sync relay infrastructure ✅

Tauri event plumbing for binary Automerge sync messages. The old `useNotebook` path still runs — this just adds the new pipes.

| Event | Direction | Payload |
|-------|-----------|---------|
| `automerge:from-daemon` | Daemon → Tauri → Frontend | `Vec<u8>` via Tauri event (base64 encoded) |
| `send_automerge_sync` | Frontend → Tauri → Daemon | `Vec<u8>` via Tauri command |
| `get_automerge_doc_bytes` | Frontend ← Tauri | `Vec<u8>` one-shot bootstrap from Tauri's replica |

- [x] `raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>` added to sync client's background task — forwards incoming `0x00` Automerge frames to channel before local application
- [x] `connect_split_with_raw_sync()` variant on `NotebookSyncClient`
- [x] Tauri spawns raw sync relay task: reads from `raw_sync_rx`, emits `automerge:from-daemon` events
- [x] `get_automerge_doc_bytes` command: exports Tauri-side `AutoCommit` doc as bytes for frontend bootstrap
- [x] `send_automerge_sync` command: receives frontend sync messages, applies via `receive_frontend_sync_message`
- [x] `frontend_peer_state` in sync client maintains separate sync state for the frontend peer (distinct from daemon peer state)
- [x] `NotebookSyncHandle.send_request()` preserved for kernel commands

### Sub-PR 2C — Hook replacement + migration (behind feature flag) 🔄

The core architectural change. `useAutomergeNotebook` replaces `useNotebook` when the feature flag is enabled.

- [x] Create `apps/notebook/src/hooks/useAutomergeNotebook.ts`
- [x] Frontend owns a local Automerge doc — all cell mutations are local `Automerge.change()` calls:
  - `addCell` — `crypto.randomUUID()`, insert via `(d.cells as any).insertAt(idx, {...})`
  - `deleteCell` — remove via `(d.cells as any).deleteAt(idx)` (last-cell guard local)
  - `updateCellSource` — `Automerge.updateText(d, ["cells", idx, "source"], newValue)`
  - Cell reorder — Automerge list move (new capability, not yet implemented)
- [x] `materializeCells()` converts Automerge doc → `CellSnapshot[]` → `NotebookCell[]`
- [x] Shared utilities extracted to `apps/notebook/src/lib/automerge-utils.ts`
- [x] Output handling: dual path — `daemon:broadcast` for real-time streaming, Automerge sync for eventual consistency
- [x] `cell:source_updated` listener updates React state only (no Automerge doc write — formatting arrives via sync)
- [x] Frontend generates sync messages after each `Automerge.change()` via `Automerge.generateSyncMessage()`
- [x] Frontend applies incoming sync messages via `Automerge.receiveSyncMessage()`
- [x] Wired into `App.tsx` behind feature flag toggle
- [x] No `invoke()` calls for cell mutations — Automerge sync is the sole transport
- [x] `notebook:updated` fallback removed — single source of truth from Automerge doc
- [x] Diagnostic logging added to daemon's `ExecuteCell` handler (cell count + available IDs on "Cell not found")
- [x] Diagnostic logging in sync relay (BEFORE/AFTER cell counts, message sizes, decode status)
- [ ] 🚧 **BLOCKED: Phantom cell bug** — execution fails because daemon doesn't have the cell IDs the frontend sees

Key implementation details:
- `import { next as Automerge } from "@automerge/automerge"` — required for v2 `updateText`/`splice` access
- `Automerge.change()` returns a new immutable doc — stored in `useRef`, React state derived via materialization
- List mutations use proxy methods inside `change()` callback: `(d.cells as any).insertAt()` / `.deleteAt()`
- Do NOT call `syncToBackend()` after `Automerge.load()` — let daemon initiate the sync exchange
- Scalar strings from Automerge are `RawString` objects — use `String(value)` for comparison

### 🚧 Blocker: Phantom cell bug

**Symptom:** Frontend loads 1 cell from doc bytes (e.g., `ee7e32a6`). After the first daemon sync response (106 bytes), a second cell appears (e.g., `ce2efbdf`) that doesn't exist in the daemon's doc or the Tauri relay's doc. The user types in this phantom cell and tries to execute — daemon returns "Cell not found."

**What we've ruled out:**
- ~~Race condition with `notebook:updated` fallback~~ — fallback removed entirely, still happens
- ~~Stale `frontend_peer_state` buffering messages~~ — deferred to `None` until `GetDocBytes`, still happens
- ~~JS v3 wire format incompatibility~~ — reverted to v2.2.x, still happens
- ~~Initial `syncToBackend()` corrupting state~~ — both with and without it, same behavior
- ~~Sync message decode failures~~ — every message decodes OK, no errors anywhere in the relay

**What the logs consistently show:**
- Tauri relay receives frontend sync messages, decodes OK, applies OK — but cell list NEVER changes (BEFORE = AFTER)
- Daemon sync responses to the frontend produce phantom cells that no Rust-side doc has
- The frontend's `Automerge.change()` calls (source edits, cell adds) produce sync messages that the relay accepts but that have no effect on the relay's doc

**Root cause hypothesis:** The three-way Automerge sync architecture (Frontend JS doc ↔ Tauri Rust doc ↔ Daemon Rust doc) with `doc.save()` bootstrap is not working. The virtual sync handshake establishes peer state for identical docs, but subsequent `Automerge.change()` calls in JS produce sync messages that the Rust `receive_sync_message` accepts silently without incorporating the changes. This may be a fundamental JS v2 ↔ Rust 0.7 sync interop issue that the unit test doesn't cover because the unit test only tests JS↔JS sync, not JS→Rust relay→Rust.

### Exploratory spikes (Phase 2 unblock)

The current approach of JS Automerge in the webview syncing through a Rust Automerge relay to the Rust daemon is stuck. Before continuing, we need to isolate whether the problem is the JS↔Rust sync interop, the relay architecture, or something else.

**Spike A: Reproduce with Python bindings**

The Python `runtimed` package uses `NotebookSyncClient` (Rust) directly — byte-for-byte identical to the daemon's Automerge. If we can create cells from Python and execute them, the Rust↔Rust sync path is confirmed working. Then we know the problem is specifically JS↔Rust.

- [ ] Connect Python `Session` to a notebook room
- [ ] `session.create_cell("print('hello')")` → verify cell appears in daemon doc
- [ ] `session.execute_cell(cell_id)` → verify execution succeeds
- [ ] If this works: JS↔Rust sync is the blocker, not the relay architecture

**Spike B: Deno FFI bindings**

Create Deno bindings that link to the same `runtimed` Rust library via FFI (similar to how `runtimed-py` links via PyO3). This gives us another directly-working Automerge peer without the JS WASM layer.

- [ ] Prototype `runtimed-deno` FFI bindings for `NotebookSyncClient`
- [ ] Connect from Deno, create cell, execute — does it work?
- [ ] If yes: confirms the problem is JS WASM Automerge, not the relay

**Spike C: Custom WASM bindings for notebook protocol**

Instead of using `@automerge/automerge` (JS WASM from a separate automerge-rs build), compile our own WASM module from the same `automerge = "0.7"` crate the daemon uses. This eliminates any version mismatch between the JS WASM and Rust automerge.

- [ ] Create a thin Rust crate that wraps `NotebookDoc` operations and compiles to WASM
- [ ] Expose: `load(bytes)`, `add_cell(...)`, `update_source(...)`, `delete_cell(...)`, `generate_sync_message(...)`, `receive_sync_message(...)`
- [ ] Test from Deno first (faster iteration than Tauri webview)
- [ ] If sync works: the problem was `@automerge/automerge` JS package using a different automerge-rs version

**Spike D: Separate Tauri window with direct Automerge**

Feature-flag a separate Tauri window that does Automerge work directly in the Rust process (not in the webview). The webview sends cell mutations as Tauri commands, the Rust side applies them to its Automerge doc and syncs to daemon. This tests whether the relay architecture works when there's no JS↔Rust sync boundary.

- [ ] Environment variable flag on the Tauri side
- [ ] Webview sends structured commands (`AddCell`, `UpdateSource`, `DeleteCell`)
- [ ] Tauri applies to its Automerge doc directly (no JS Automerge involved)
- [ ] If execution works: confirms the problem is JS→Rust sync, not the relay architecture

**Decision point:** After spikes, choose one of:
1. **Spike C worked** → Ship custom WASM bindings (guaranteed version match)
2. **Spike D worked** → Keep mutations as Tauri commands, frontend is a thin view (hybrid approach)
3. **Spike A/B showed Rust↔Rust works** → Focus debugging on JS WASM sync messages specifically

### Sub-PR 2D — Cleanup (after phantom cell bug is resolved and feature flag is flipped)

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