# Phase 2: Frontend Automerge Document — Implementation Plan

## Context

The current notebook architecture is "RPC-with-optimistic-UI": every cell mutation (add, delete, edit) fires an `invoke()` call to Tauri, which sends it to the daemon, which applies it to the canonical Automerge doc, and broadcasts the result back. The frontend is a stateless read replica with no Automerge library of its own — it receives materialized `CellSnapshot[]` arrays and reconciles React state.

This creates unnecessary latency for operations that could be instant. Phase 2 gives the frontend its own Automerge document, making mutations local-first. The Tauri process transitions toward being a relay, and the frontend becomes a true CRDT peer.

**Key decisions made:**
- **Tauri role**: Transitional — keep Tauri's Automerge replica during migration, remove in follow-up
- **Output flow**: Dual path — keep `daemon:broadcast` for real-time streaming, Automerge sync for eventual consistency
- **Migration strategy**: Feature flag toggle — build `useAutomergeNotebook` alongside `useNotebook`
- **Scope**: 4 independently-shippable sub-PRs

---

## Implementation Progress

- **PR 1**: COMMITTED (8e1fb02) — Automerge WASM, schema, feature flag
- **PR 2**: COMMITTED (2c750c4) — Binary sync relay, doc bytes commands
- **PR 3**: COMMITTED (e594013) — useAutomergeNotebook hook, automerge-utils, dispatch toggle
- **PR 4**: Not started (planned for after PR 3 validation)

All three PRs are on branch `claude/phase-2-planning-1F2j8`.

### Verification Results (PRs 1–3)
- `cargo fmt` — clean
- `biome check` — clean (55 files, 0 fixes)
- `cargo clippy -p runtimed` — clean
- `cargo test -p runtimed` — 15/15 passed
- `cargo test -p kernel-launch` — 8/8 passed
- `vite build` — succeeds
- `cargo clippy -p notebook` — skipped (requires `runtimed` binary artifact, CI-only)
- E2E tests — no test suite exists in repo yet

---

## PR 1: Automerge WASM + TypeScript Schema

**Goal**: Get `@automerge/automerge` working in the frontend build with the TypeScript schema mirroring the Rust `NotebookDoc`.

### 1.1 — Install Automerge WASM

- Add `@automerge/automerge` to `apps/notebook/package.json`
- Add `vite-plugin-wasm` and `vite-plugin-top-level-await` to dev deps
- Configure Vite plugins in `apps/notebook/vite.config.ts`:
  ```ts
  import wasm from "vite-plugin-wasm";
  import topLevelAwait from "vite-plugin-top-level-await";
  // Add to plugins array
  ```
- Verify WASM bundle loads correctly in dev and build modes
- Check gzip size target: Automerge WASM should be <250KB gzipped

### 1.2 — Define TypeScript Schema

Create `apps/notebook/src/lib/automerge-schema.ts` mirroring the Rust doc schema from `crates/runtimed/src/notebook_doc.rs`:

```ts
// Mirror of Rust NotebookDoc schema (notebook_doc.rs lines 10-24)
// ROOT/
//   notebook_id: string
//   cells: AutomergeList<CellDoc>
//   metadata: AutomergeMap { runtime: string, notebook_metadata: string }

interface CellDoc {
  id: string;                    // cell UUID
  cell_type: string;             // "code" | "markdown" | "raw"
  source: string;                // Automerge Text CRDT
  execution_count: string;       // "5" or "null"
  outputs: string[];             // List of JSON strings or manifest hashes
}

interface NotebookSchema {
  notebook_id: string;
  cells: CellDoc[];
  metadata: { runtime: string; notebook_metadata: string };
}
```

### 1.3 — Add Feature Flag

Create `apps/notebook/src/lib/feature-flags.ts`:
```ts
export const USE_AUTOMERGE_FRONTEND =
  localStorage.getItem("USE_AUTOMERGE_FRONTEND") === "true";
```

### Files modified:
- `apps/notebook/package.json` — add deps
- `apps/notebook/vite.config.ts` — add WASM plugins
- `apps/notebook/src/lib/automerge-schema.ts` — **new file**
- `apps/notebook/src/lib/feature-flags.ts` — **new file**

---

## PR 2: Binary Sync Relay + Initialization Protocol

**Goal**: Add Tauri infrastructure to relay raw Automerge sync bytes between frontend and daemon, and send initial doc bytes to the frontend.

### 2.1 — New Tauri Event Channels

Add two new event channels in `crates/notebook/src/lib.rs`:

- **`automerge:from-daemon`** — Tauri emits raw Automerge sync message bytes to the frontend (as `Vec<u8>` / base64)
- **`automerge:to-daemon`** — Frontend sends Automerge sync messages to Tauri via a new command

Since Tauri events carry JSON payloads, binary sync messages will need base64 encoding. Add a new Tauri command:

```rust
#[tauri::command]
async fn send_automerge_sync(
    sync_message: Vec<u8>,  // base64-decoded by Tauri's serde
    state: State<'_, NotebookSyncState>,
) -> Result<(), String> {
    // Forward raw sync message to daemon via the existing connection
}
```

### 2.2 — Relay Sync Messages from Daemon

In the existing receiver task (lib.rs:649-705), alongside emitting `notebook:updated`, also emit the raw sync bytes on `automerge:from-daemon`. The `NotebookSyncClient` already receives Automerge sync messages internally — we need to expose these raw bytes before the client deserializes them into `CellSnapshot[]`.

This requires a change in `notebook_sync_client.rs`: add an option to also forward raw sync messages to a channel, not just materialized cell snapshots. The `NotebookSyncReceiver` currently only yields `NotebookSyncUpdate` (materialized cells). We need a new variant or parallel channel for raw bytes.

**Approach**: Add a `raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>` to the sync client's background task. When set, it forwards incoming Automerge sync messages (the binary `0x00` frames) to this channel before applying them locally. The Tauri side creates this channel and listens on it to emit `automerge:from-daemon`.

**Key implementation detail — Frontend peer state tracking**: The daemon sends sync messages to the Tauri doc, not the frontend doc directly. This is solved by maintaining a separate `frontend_peer_state: sync::State` in the background task. When daemon changes arrive, a sync message is generated from Tauri doc → frontend doc. When the frontend sends sync, it's applied to the Tauri doc using `frontend_peer_state` and relayed to the daemon.

### 2.3 — Initialization: Send Doc Bytes to Frontend

Add a new Tauri command `get_automerge_doc_bytes`:

```rust
#[tauri::command]
async fn get_automerge_doc_bytes(
    state: State<'_, NotebookSyncState>,
) -> Result<Vec<u8>, String> {
    // Export the Tauri-side Automerge doc as bytes
    // Frontend will load this to initialize its local doc
}
```

This leverages the transitional architecture — Tauri still has its replica, so the frontend can bootstrap from it. The Automerge `save()` function exports the full doc state.

**Flow**:
1. Tauri loads .ipynb, creates Automerge doc, syncs with daemon (existing behavior)
2. Frontend calls `invoke("get_automerge_doc_bytes")` after `daemon:ready`
3. Frontend calls `Automerge.load(bytes)` to create its local doc
4. Frontend begins bidirectional sync via the relay channels

### Files modified:
- `crates/notebook/src/lib.rs` — new commands (`get_automerge_doc_bytes`, `send_automerge_sync`), relay events
- `crates/runtimed/src/notebook_sync_client.rs` — `GetDocBytes`, `ReceiveFrontendSyncMessage` commands, `connect_split_with_raw_sync`, frontend peer state tracking

---

## PR 3: `useAutomergeNotebook` Hook + Feature Flag Toggle

**Goal**: Build the new hook that owns a local Automerge document. All cell mutations happen locally first, then sync to daemon. Toggled by feature flag, coexisting with `useNotebook`.

### 3.1 — Core Hook: `useAutomergeNotebook`

Create `apps/notebook/src/hooks/useAutomergeNotebook.ts`:

**State**:
- `docRef = useRef<Automerge.Doc<NotebookSchema>>()` — the local Automerge doc
- `cells` — derived from `docRef` via materialization (same `NotebookCell[]` type)
- `syncStateRef` — Automerge `SyncState` for the daemon peer

**Initialization**:
```ts
useEffect(() => {
  invoke<number[]>("get_automerge_doc_bytes").then(bytes => {
    const doc = Automerge.load(new Uint8Array(bytes));
    docRef.current = doc;
    setCells(materializeCells(doc));
  });
}, []);
```

**Local mutations** (instant, no RPC):
```ts
const updateCellSource = (cellId: string, source: string) => {
  docRef.current = Automerge.change(docRef.current, doc => {
    const cell = doc.cells.find(c => c.id === cellId);
    if (cell) Automerge.updateText(doc, ["cells", idx, "source"], source);
  });
  setCells(materializeCells(docRef.current));
  syncToBackend();
};

const addCell = (cellType, afterCellId) => {
  const cellId = crypto.randomUUID();
  docRef.current = Automerge.change(docRef.current, doc => {
    const idx = afterCellId ? doc.cells.findIndex(c => c.id === afterCellId) + 1 : 0;
    Automerge.insertAt(doc, ["cells"], idx, { id: cellId, cell_type: cellType, ... });
  });
  setCells(materializeCells(docRef.current));
  syncToBackend();
};
```

**Sync with daemon**:
```ts
const syncToBackend = () => {
  const [newSyncState, message] = Automerge.generateSyncMessage(
    docRef.current, syncStateRef.current
  );
  syncStateRef.current = newSyncState;
  if (message) {
    invoke("send_automerge_sync", { syncMessage: Array.from(message) });
  }
};

// Receive from daemon
webview.listen<number[]>("automerge:from-daemon", (event) => {
  const message = new Uint8Array(event.payload);
  const [newDoc, newSyncState] = Automerge.receiveSyncMessage(
    docRef.current, syncStateRef.current, message
  );
  docRef.current = newDoc;
  syncStateRef.current = newSyncState;
  setCells(materializeCells(newDoc));

  // May need to send a response
  syncToBackend();
});
```

**Materialization function**:
Reuse existing `cellSnapshotsToNotebookCells` logic from `useNotebook.ts` — extracted as a shared utility in `automerge-utils.ts`. The Automerge doc cells already match the `CellSnapshot` shape, so we can convert directly.

### 3.2 — Output Handling (Dual Path)

Keep `daemon:broadcast` output events flowing through `useDaemonKernel.ts` unchanged. The `appendOutput`, `updateOutputByDisplayId`, and `setExecutionCount` callbacks continue to update React state directly for real-time display.

When Automerge sync messages arrive (which include the daemon's output writes), the materialization step reconciles — but the broadcast path gives us real-time streaming while Automerge provides eventual consistency.

### 3.3 — Feature Flag Toggle

Created `useNotebookDispatch.ts` that selects the hook at module level (not inside a component, which would violate React rules of hooks):

```ts
import { USE_AUTOMERGE_FRONTEND } from "../lib/feature-flags";
import { useAutomergeNotebook } from "./useAutomergeNotebook";
import { useNotebook } from "./useNotebook";

export const useNotebookDispatch = USE_AUTOMERGE_FRONTEND
  ? useAutomergeNotebook
  : useNotebook;
```

Both hooks expose the same return type, so the rest of App.tsx is unchanged.

### 3.4 — Legacy Compatibility

During the transition, `useAutomergeNotebook` still fires `invoke("add_cell")` and `invoke("delete_cell")` for backend compatibility, since the daemon expects these operations through the existing RPC path.

### Files created/modified:
- `apps/notebook/src/hooks/useAutomergeNotebook.ts` — **new file**, core local-first hook (535 lines)
- `apps/notebook/src/lib/automerge-utils.ts` — **new file**, shared output resolution utilities (269 lines)
- `apps/notebook/src/hooks/useNotebookDispatch.ts` — **new file**, feature flag dispatch (16 lines)
- `apps/notebook/src/App.tsx` — use `useNotebookDispatch` instead of `useNotebook`

---

## PR 4: Cleanup Old Code Paths (NOT STARTED)

**Goal**: Once the Automerge frontend is validated, remove the legacy path and make Tauri a true relay.

### 4.1 — Remove `useNotebook`
- Delete `apps/notebook/src/hooks/useNotebook.ts`
- Remove feature flag infrastructure
- `useAutomergeNotebook` becomes the only hook

### 4.2 — Remove Tauri Automerge Replica
- Remove `NotebookSyncClient`'s local `AutoCommit` doc
- Tauri no longer materializes `CellSnapshot[]` or emits `notebook:updated`
- Remove `refresh_from_automerge` command
- Remove `load_notebook` command (frontend loads from its own doc)
- Remove cell mutation commands: `update_cell_source`, `add_cell`, `delete_cell`

### 4.3 — Simplify Sync Client
- `NotebookSyncClient` becomes a pure relay: reads frames from daemon socket, forwards to frontend event; reads from frontend command, forwards to daemon socket
- No Automerge dependency in `crates/notebook` at all

### Files to modify:
- `apps/notebook/src/hooks/useNotebook.ts` — **delete**
- `apps/notebook/src/lib/feature-flags.ts` — **delete**
- `apps/notebook/src/App.tsx` — remove flag logic
- `crates/notebook/src/lib.rs` — remove obsolete commands
- `crates/runtimed/src/notebook_sync_client.rs` — simplify to pure relay

### Verification:
- All functionality works as in PR 3
- Tauri process memory footprint reduced (no duplicate doc)
- `notebook:updated` event no longer emitted (replaced by `automerge:from-daemon`)

---

## Risk Considerations

| Risk | Mitigation |
|------|-----------|
| WASM bundle size | Check with `rollup-plugin-visualizer` in PR 1; fail build if >250KB gzip |
| Performance on large notebooks | Profile `Automerge.change()` + materialization in PR 3; test with 100/500/1000 cells |
| Text editing latency | `Automerge.updateText()` uses Myers diff — test with rapid typing |
| Sync message ordering | Automerge protocol handles this natively; test with concurrent edits |
| Output deduplication | Dual path means outputs may appear twice briefly; debounce in materialization |
| Base64 overhead for binary relay | ~33% overhead; acceptable for sync messages (typically <10KB) |

---

## Key Files Reference

| File | Current Role | Phase 2 Change |
|------|-------------|----------------|
| `apps/notebook/src/hooks/useNotebook.ts` | Cell state + RPC mutations | Kept behind flag, then deleted |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | **New** — local-first cell state | Core of Phase 2 |
| `apps/notebook/src/hooks/useNotebookDispatch.ts` | **New** — feature flag dispatch | Selects hook at module level |
| `apps/notebook/src/lib/automerge-utils.ts` | **New** — shared output resolution | Extracted from useNotebook |
| `apps/notebook/src/lib/automerge-schema.ts` | **New** — TypeScript schema | Mirrors Rust NotebookDoc |
| `apps/notebook/src/lib/feature-flags.ts` | **New** — localStorage flag | `USE_AUTOMERGE_FRONTEND` |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Kernel ops + broadcast | Unchanged |
| `apps/notebook/src/App.tsx` | Hook composition | Uses useNotebookDispatch |
| `apps/notebook/vite.config.ts` | Build config | Added WASM plugins |
| `crates/notebook/src/lib.rs` | Tauri commands + sync | Added relay commands |
| `crates/runtimed/src/notebook_sync_client.rs` | Automerge sync client | Added raw byte forwarding + frontend peer state |
| `crates/runtimed/src/notebook_doc.rs` | Automerge doc schema | Reference for TS schema |
| `crates/runtimed/src/connection.rs` | Frame protocol | Reference (0x00 = sync) |
