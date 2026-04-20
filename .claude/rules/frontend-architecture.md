---
paths:
  - apps/notebook/src/**
---

# Frontend Architecture

## Directory Layout

```
/
├── src/                          <- Shared components (path alias @/)
│   ├── bindings/                 <- TypeScript types generated from Rust
│   ├── components/
│   │   ├── cell/                 <- Cell container, controls, execution count
│   │   ├── editor/               <- CodeMirror wrappers, extensions, themes
│   │   ├── isolated/             <- Iframe security isolation
│   │   ├── outputs/              <- Output renderers (MediaRouter, AnsiOutput, etc.)
│   │   ├── widgets/              <- ipywidgets and anywidget implementations
│   │   └── ui/                   <- shadcn components (Button, Dialog, etc.)
│   ├── hooks/                    <- Shared hooks (useSyncedSettings, useTheme)
│   ├── isolated-renderer/        <- Code that runs INSIDE isolated iframe
│   ├── lib/                      <- Shared utilities (utils.ts with cn())
│   └── styles/                   <- Global stylesheets
│
├── apps/
│   ├── notebook/src/             <- Notebook app (path alias ~/)
│   │   ├── components/           <- App-specific components (toolbar, banners)
│   │   ├── contexts/             <- React contexts (PresenceContext)
│   │   ├── hooks/                <- Notebook-specific hooks
│   │   ├── lib/                  <- App-specific utilities
│   │   ├── wasm/                 <- WASM bindings (runtimed-wasm)
│   │   ├── App.tsx               <- Root component
│   │   └── types.ts              <- App types
│   ├── notebook/feedback/         <- Feedback sub-app
│   ├── notebook/onboarding/      <- Onboarding sub-app
│   ├── notebook/settings/        <- Settings sub-app
│   └── notebook/upgrade/         <- Upgrade sub-app
```

## Path Aliases

| Alias | Resolves To | Use For |
|-------|-------------|---------|
| `@/*` | `../../src/*` | Shared components, hooks, utilities |
| `~/*` | `./src/*` | App-specific code |

## Shared vs App-Specific

Put code in `src/` (shared) when:
- It is a pure UI component with no Tauri/daemon dependencies
- It could be reused by future apps
- It is a generic utility (cn(), theme helpers)

Put code in `apps/notebook/src/` when:
- It uses the `NotebookHost` interface (see below)
- It interacts with the daemon (kernel execution, notebook sync)
- It is specific to the notebook editing experience

## NotebookHost (the Tauri/Electron abstraction)

The frontend goes through `@nteract/notebook-host` for every host-platform
side effect. **Do not import `@tauri-apps/*` directly** — that's what the
host abstraction exists to prevent. The only places that may import
`@tauri-apps/*` are `packages/notebook-host/src/tauri/` and the daemon
relay glue in `apps/notebook/src/lib/` (frame-types.ts, tauri-transport).

| Namespace | Role |
|-----------|------|
| `host.transport` | `NotebookTransport` shared by SyncEngine / NotebookClient |
| `host.daemon` | `isConnected`, `reconnect`, `getInfo`, `getReadyInfo` (cached `daemon:ready` payload) |
| `host.daemonEvents` | `onReady` / `onProgress` / `onDisconnected` / `onUnavailable` subscriptions (webview events) |
| `host.relay` | `notifySyncReady()` outbound signal |
| `host.blobs` | `port()` — daemon blob-server port |
| `host.trust` | `verify()`, `approve()` |
| `host.deps` | `checkTyposquats()` (deps-edit API will grow) |
| `host.notebook` | `applyPathChanged` / `markClean` (legacy shadow, going away) |
| `host.window` | `getTitle` / `setTitle` / `onFocusChange` |
| `host.system` | `getGitInfo`, `getUsername` |
| `host.dialog` | `openFile` / `saveFile` (plugin-dialog wrap) |
| `host.externalLinks` | `open(url)` (plugin-shell wrap) |
| `host.updater` | `check()` (plugin-updater wrap) |
| `host.commands` | Typed command bus — menus + keyboard + (future) palette |
| `host.log` | `debug/info/warn/error` (plugin-log wrap) |

**React code** — use `const host = useNotebookHost();` from `@nteract/notebook-host`.

**Module-level helpers** (can't call hooks) — use the setter pattern:
- `setLoggerHost(host)` in `logger.ts`
- `setBlobPortHost(host)` in `blob-port.ts`
- `setOpenUrlHost(host)` in `open-url.ts`
- `setMetadataTransport(host.transport)` in `notebook-metadata.ts`

All setters are called once from `main.tsx` right after `createTauriHost()`.

**Still-direct `invoke(...)` calls** live in `notebook-file-ops.ts`,
`useUpdater.ts`, `useDaemonKernel.ts`, `useDependencies{,Conda,Pixi,Deno}.ts`,
`useHistorySearch.ts`, `kernel-completion.ts`, `PoolErrorBanner.tsx`.
These are the `*_via_daemon` thin wrappers + env-detection + save/clone
helpers. They'll migrate to `transport.sendRequest(NotebookRequest)` /
daemon-owned detection in subsequent PRs. See
`.context/tauri-daemon-audit.md` for the full queue.

**Canonical surface**: `packages/notebook-host/src/types.ts`.
**Tauri implementation**: `packages/notebook-host/src/tauri/index.ts`.

## Key Shared Components

| Component | Location | Role |
|-----------|----------|------|
| `CellContainer` | `src/components/cell/` | Wrapper with selection, focus, drag handles |
| `CellControls` | `src/components/cell/` | Play button, cell type indicator |
| `OutputArea` | `src/components/cell/` | Cell output rendering (routes to isolated frames) |
| `MediaRouter` | `src/components/outputs/` | Output type dispatch by MIME type |
| `CodeMirror editor` | `src/components/editor/` | Main editor component |

## Key App-Specific Hooks

| Hook | Role |
|------|------|
| `useAutomergeNotebook` | Owns WASM NotebookHandle, `scheduleMaterialize`, `CellChangeset` dispatch |
| `useDaemonKernel` | Kernel execution, status broadcasts, widget comm routing |
| `usePresence` | Remote cursor/selection tracking via presence frames |
| `useDependencies` | UV dependency management |
| `useCondaDependencies` | Conda dependency management |
| `usePixiDetection` | Pixi project detection (pixi.toml is the source of truth) |
| `usePoolState` | Daemon pool state via PoolDoc sync |
| `useCrdtBridge` | CodeMirror ↔ CRDT character-level sync |
| `useManifestResolver` | Resolves blob hashes to output data |
| `useCellKeyboardNavigation` | Arrow keys, enter/escape modes |
| `useEditorRegistry` | CodeMirror editor instance registry |
| `useGlobalFind` | Global find-and-replace across cells |
| `useTrust` | Notebook trust verification state |

## Data Flow

The frontend has a single ingress point for daemon frames. All data flows through WASM demux before reaching React:

1. Tauri relay sends `notebook:frame` events containing typed frame bytes.
2. `useAutomergeNotebook` receives frames, passes to WASM `receive_frame()` for demux.
3. WASM returns `FrameEvent[]` with sync results, broadcasts, and presence.
4. Sync results include `CellChangeset` with field-level granularity.
5. Broadcasts and presence are dispatched via in-memory frame bus (`notebook-frame-bus.ts`).

## Incremental Sync Pipeline

1. **useAutomergeNotebook** -- Single ingress. WASM `receive_frame()` returns `CellChangeset` with per-field flags. Broadcasts dispatched via `emitBroadcast()` / `emitPresence()`.

2. **scheduleMaterialize** -- Coalesces within 32ms via `mergeChangesets()`:
   - Structural changes (add/remove/reorder) -> full `cellSnapshotsToNotebookCells()` from `get_cells_json()`
   - Output changes -> per-cell cache-aware resolution (cache hits use `materializeCellFromWasm()`, misses resolve async)
   - Source/metadata/execution_count only -> per-cell `materializeCellFromWasm()` via O(1) WASM accessors

3. **Split cell store** (`notebook-cells.ts`):
   - `useCell(id)` -- re-renders only when that specific cell changes
   - `useCellIds()` -- re-renders only on structural changes
   - `updateCellById()` -- O(1) map update, notifies only that cell's subscribers
   - `replaceNotebookCells()` -- full replacement with `cellsEqual()` diffing

4. **useDaemonKernel / useEnvProgress** -- Subscribe via `subscribeBroadcast()` from frame bus

5. **usePresence** -- Subscribes via `subscribePresence()` from frame bus. Maintains peer map.

6. **cursor-registry.ts** -- Independent frame bus subscriber. Dispatches `setRemoteCursors()`/`setRemoteSelections()` as CodeMirror `StateEffect`s directly to `EditorView` instances, bypassing React.

## Mutation Flow

Cell mutations go through the WASM handle for instant response. Source edits are batched via `engine.scheduleFlush()` (20ms debounce), with `engine.flush()` before execute/save. Fast path for typing: `updateCellSource()` -> WASM `update_source()` -> `updateCellById()` (one cell, one subscriber) -> debounced sync.

Execution requests go to the daemon via dedicated Tauri commands (`execute_cell_via_daemon`, etc.). These are scheduled to migrate onto `host.transport.sendRequest(NotebookRequest)` in a follow-up; for now they still `invoke(...)` directly.

## CellChangeset Types

Defined in `notebook-doc/src/diff.rs`, with TypeScript types in `packages/runtimed/src/cell-changeset.ts` (re-exported via `apps/notebook/src/lib/cell-changeset.ts`):
- `CellChangeset` -- `{ changed, added, removed, order_changed }`
- `ChangedCell` -- `{ cell_id, fields }` where `fields` has boolean flags: `source`, `outputs`, `execution_count`, `cell_type`, `metadata`, `position`, `resolved_assets`
- `mergeChangesets()` -- union semantics for the coalescing window

## Key Files

| File | Role |
|------|------|
| `apps/notebook/src/App.tsx` | Root component, provider setup |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM handle, scheduleMaterialize, CellChangeset |
| `apps/notebook/src/lib/materialize-cells.ts` | WASM -> React conversion |
| `apps/notebook/src/lib/notebook-cells.ts` | Split cell store, per-cell subscriptions |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory pub/sub for broadcasts and presence |
| `apps/notebook/src/lib/frame-types.ts` | Frame type constants + `sendFrame()` binary IPC |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Kernel execution, broadcast handling |
| `apps/notebook/src/hooks/usePresence.ts` | Remote presence tracking |
| `src/components/outputs/media-router.tsx` | Output type dispatch |
| `src/components/editor/codemirror-editor.tsx` | Main editor |
