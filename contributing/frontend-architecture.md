# Frontend Architecture

This guide explains the frontend code organization and how shared components relate to the notebook application.

## Directory Layout

```
/
├── src/                          ← Shared components (path alias @/)
│   ├── bindings/                 ← TypeScript types generated from Rust (see typescript-bindings.md)
│   ├── components/
│   │   ├── cell/                 ← Cell container, controls, execution count
│   │   ├── editor/               ← CodeMirror wrappers, extensions, themes
│   │   ├── isolated/             ← Iframe security isolation (IsolatedFrame, CommBridgeManager)
│   │   ├── outputs/              ← Output renderers (MediaRouter, AnsiOutput, etc.)
│   │   ├── widgets/              ← ipywidgets and anywidget implementations
│   │   └── ui/                   ← shadcn components (Button, Dialog, etc.)
│   ├── hooks/                    ← Shared hooks (useSyncedSettings, useTheme)
│   ├── isolated-renderer/        ← Code that runs INSIDE isolated iframe
│   ├── lib/                      ← Shared utilities (utils.ts with cn())
│   └── styles/                   ← Global stylesheets
│
├── apps/
│   ├── notebook/src/             ← Notebook app (path alias ~/)
│   │   ├── components/           ← App-specific components (toolbar, banners)
│   │   ├── contexts/             ← React contexts (PresenceContext)
│   │   ├── hooks/                ← Notebook-specific hooks (useDaemonKernel, etc.)
│   │   ├── lib/                  ← App-specific utilities (materialize-cells.ts)
│   │   ├── wasm/                 ← WASM bindings (runtimed-wasm)
│   │   ├── App.tsx               ← Root component
│   │   └── types.ts              ← App types
│   ├── notebook/feedback/        ← Feedback sub-app
│   ├── notebook/onboarding/      ← Onboarding sub-app (separate HTML entry point)
│   ├── notebook/settings/        ← Settings sub-app
│   └── notebook/upgrade/         ← Upgrade sub-app
│

```

## Path Aliases

The notebook app uses two path aliases configured in `apps/notebook/tsconfig.json`:

| Alias | Resolves To | Use For |
|-------|-------------|---------|
| `@/*` | `../../src/*` | Shared components, hooks, utilities |
| `~/*` | `./src/*` | App-specific code |

Example imports:
```tsx
import { CellContainer } from "@/components/cell";      // shared
import { useDaemonKernel } from "~/hooks/useDaemonKernel";  // app-specific
```

## Shared vs App-Specific

**Put code in `src/` (shared) when:**
- It's a pure UI component with no Tauri/daemon dependencies
- It could be reused by future apps
- It's a generic utility (cn(), theme helpers)

**Put code in `apps/notebook/src/` when:**
- It uses the `NotebookHost` interface (see below)
- It interacts with the daemon (kernel execution, notebook sync)
- It's specific to the notebook editing experience

## NotebookHost — the Tauri / Electron abstraction

Every host-platform side effect (Tauri IPC, plugin calls, window chrome)
flows through `@nteract/notebook-host`. The notebook frontend should
**never import `@tauri-apps/*` directly** — it goes through `host.*`
methods so the same React tree can run under Tauri today and Electron
next. The abstraction lives in `packages/notebook-host/src/`.

| Namespace | Purpose |
|-----------|---------|
| `host.transport` | `NotebookTransport` shared by SyncEngine / NotebookClient |
| `host.daemon` | `isConnected`, `reconnect`, `getInfo`, `getReadyInfo` (cached `daemon:ready` payload for late subscribers) |
| `host.daemonEvents` | `onReady` / `onProgress` / `onDisconnected` / `onUnavailable` (webview-event subscriptions) |
| `host.relay` | `notifySyncReady()` outbound signal |
| `host.blobs` | `port()` — daemon blob-server HTTP port |
| `host.trust` | `verify()` / `approve()` |
| `host.deps` | Dependency validation (`checkTyposquats`) |
| `host.notebook` | `applyPathChanged` / `markClean` (legacy shadow, slated for removal) |
| `host.window` | `getTitle` / `setTitle` / `onFocusChange` |
| `host.system` | `getGitInfo`, `getUsername` |
| `host.dialog` | `openFile` / `saveFile` (plugin-dialog wrap) |
| `host.externalLinks` | `open(url)` (plugin-shell wrap) |
| `host.updater` | `check()` (plugin-updater wrap) |
| `host.commands` | Typed command bus (menus + keyboard + future palette) |
| `host.log` | `debug/info/warn/error` (plugin-log wrap) |

**React code** — `const host = useNotebookHost();` from `@nteract/notebook-host`.

**Module-level helpers** (no hooks) — the setter pattern, called once from
`main.tsx` after `createTauriHost()`:
- `setLoggerHost(host)` — `logger.ts`
- `setBlobPortHost(host)` — `blob-port.ts`
- `setOpenUrlHost(host)` — `open-url.ts`
- `setMetadataTransport(host.transport)` — `notebook-metadata.ts`

**Not yet migrated** — a pile of `invoke(...)` calls for kernel / save /
clone / dependency-detection. Those are slated to swap onto
`host.transport.sendRequest(NotebookRequest)` (direct protocol dispatch)
and daemon-owned file detection in subsequent PRs. See
`.context/tauri-daemon-audit.md` for the full audit + migration queue.

**Canonical surface**: `packages/notebook-host/src/types.ts`.
**Tauri implementation**: `packages/notebook-host/src/tauri/index.ts`.

## Key Shared Components

### Cell Components (`src/components/cell/`)

| Component | Role |
|-----------|------|
| `CellContainer` | Wrapper with selection, focus, drag handles |
| `CellControls` | Play button, cell type indicator |
| `CellHeader` | Execution count display |
| `CellBetweener` | Insert-cell affordance between cells |
| `CellTypeButton` | Cell type toggle (code/markdown) |
| `CellTypeSelector` | Dropdown for switching cell types |
| `PlayButton` | Execute cell action |
| `CompactExecutionButton` | Inline run button for compact layouts |
| `ExecutionCount` | `[n]` execution counter display |
| `ExecutionStatus` | Running/queued/error indicator |
| `OutputArea` | Cell output rendering (routes to isolated frames) |
| `CollaboratorAvatars` | Presence avatars for multi-user editing |
| `PresenceBookmarks` | Gutter marks showing collaborator positions |
| `RuntimeHealthIndicator` | Kernel connection health badge |
| `gutter-colors.ts` | Color assignment for collaborator gutters |

### Editor (`src/components/editor/`)

CodeMirror integration with Jupyter-specific extensions:
- `codemirror-editor.tsx` — Main editor component
- `extensions.ts` — Keybindings, line numbers, bracket matching
- `languages.ts` — Python, Markdown, SQL syntax (Lezer parsers; replaced Prism in #1742)
- `themes.ts` — Light and dark themes (in-tree, no external CodeMirror theme dependency)

Markdown rendering uses the same Lezer parsers for static highlighting of code fences, so the editor and the rendered preview agree on tokens without bundling a second highlighter.

### Outputs (`src/components/outputs/`)

| Renderer | MIME Types |
|----------|------------|
| `AnsiOutput` | `text/plain` with ANSI codes |
| `HtmlOutput` | `text/html` |
| `ImageOutput` | `image/png`, `image/jpeg`, etc. |
| `MarkdownOutput` | `text/markdown` |
| `JsonOutput` | `application/json` |
| `SvgOutput` | `image/svg+xml` |
| `MediaRouter` | Dispatches to appropriate renderer |

### Widgets (`src/components/widgets/`)

See [widget-development.md](widget-development.md) for internals.

### Isolated Renderer (`src/components/isolated/`)

Security boundary for untrusted HTML/widget outputs. See [iframe-isolation.md](iframe-isolation.md).

## Key App-Specific Hooks

| Hook | Role |
|------|------|
| `useAutomergeNotebook` | Owns WASM NotebookHandle, `scheduleMaterialize`, `CellChangeset` dispatch |
| `useDaemonKernel` | Kernel execution and ephemeral runtime event callbacks |
| `usePresence` | Remote cursor/selection tracking via presence frames |
| `useEnvProgress` | RuntimeStateDoc-backed environment progress projection |
| `useDependencies` | UV dependency management |
| `useCondaDependencies` | Conda dependency management |
| `useDenoConfig` | Deno config detection plus flexible-npm-imports toggle |
| `useManifestResolver` | Resolves blob hashes to output data |
| `useCellKeyboardNavigation` | Arrow keys, enter/escape modes |
| `useEditorRegistry` | CodeMirror editor instance registry |
| `useGitInfo` | Git branch/status for the notebook file |
| `useGlobalFind` | Global find-and-replace across cells |
| `useHistorySearch` | Kernel input history search |
| `useTrust` | Notebook trust verification state |
| `useUpdater` | App update checking and installation |
| `usePixiDetection` | Pixi project detection (pixi.toml is the source of truth) |
| `usePoolState` | Daemon pool state |
| `useCrdtBridge` | CodeMirror ↔ CRDT character-level sync |

## Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│ Frontend                                                        │
│                                                                 │
│  Tauri relay ── "notebook:frame" ──► useAutomergeNotebook       │
│                                      (WASM receive_frame demux) │
│                                        │          │         │   │
│                   sync_applied ────────┘          │         │   │
│                   + CellChangeset                 │         │   │
│                          ▼                        │         │   │
│                   scheduleMaterialize              │         │   │
│                   (32ms coalesce)       emitBroadcast emitPresence
│                          │             (frame bus)  │  (frame bus)
│                   ┌──────┴──────┐             │     │       │   │
│                   │ structural? │             ▼     │       │   │
│                   │             │     ┌──────────────┐      │   │
│                   └──┬──────┬───┘     │useDaemonKernel│     │   │
│             full ◄───┘      └───► per-cell            │     │   │
│          materialize-     materialize-  useEnvProgress │     │   │
│          Cells()          CellFromWasm  └──────┬───────┘     │   │
│                      (cache-aware for          │      │      │   │
│                       output changes)          │      │      │   │
│                   │           │                │      ▼      │   │
│                   ▼           ▼                │  usePresence │   │
│             ┌────────────────────┐             │      │      │   │
│             │ Split Cell Store   │             │      │      │   │
│             │ useCell(id)        │             │      │      │   │
│             │ useCellIds()       │             │      │      │   │
│             └────────┬───────────┘             │      │      │   │
│                      ▼                         ▼      ▼      │   │
│  ┌─────────────────────────────────────────────────────────┐ │   │
│  │ React Components (React.memo per cell)                   │ │   │
│  │ CellRenderer → useCell(id) → CodeCell/MarkdownCell       │ │   │
│  └─────────────────────────────────────────────────────────┘ │   │
└─────────────────────────────────────────────────────────────────┘
```

### Incremental sync pipeline

1. **useAutomergeNotebook** — Single ingress point. Listens for `notebook:frame`, demuxes via WASM `receive_frame()`, applies sync locally. WASM returns a `CellChangeset` with field-level granularity (which cells changed, which fields). Broadcasts and presence are dispatched via in-memory frame bus (`emitBroadcast()` / `emitPresence()` from `notebook-frame-bus.ts`).

2. **scheduleMaterialize** — Coalesces sync frames within a 32ms window via `mergeChangesets()`, then dispatches:
   - **Structural changes** (cells added/removed/reordered) → full `cellSnapshotsToNotebookCells()` from `get_cells_json()`
   - **Output changes** → per-cell cache-aware resolution: cache hits use fast sync path via `materializeCellFromWasm()`; cache misses resolve just that cell async
   - **Source/metadata/execution_count only** → per-cell `materializeCellFromWasm()` using O(1) WASM accessors (`get_cell_source()`, `get_cell_type()`, etc.)

3. **Split cell store** (`notebook-cells.ts`) — `Map<id, NotebookCell>` + ordered ID list with independent subscriber channels:
   - `useCell(id)` — re-renders only when that specific cell changes
   - `useCellIds()` — re-renders only on structural changes (add/remove/reorder)
   - `updateCellById()` — O(1) map update, notifies only that cell's subscribers
   - `replaceNotebookCells()` — full replacement with `cellsEqual()` diffing to preserve object identity for unchanged cells

4. **Runtime state projection** — `useDaemonKernel` still consumes ephemeral broadcast events, while persistent kernel/env/project state is projected from RuntimeStateDoc through `runtime-state.ts`, `project-runtime-stores.ts`, and hooks such as `useEnvProgress`.

5. **usePresence** — Subscribes via `subscribePresence()` from the frame bus. Maintains a React-accessible peer map with `cursorsForCell()`/`selectionsForCell()` queries.

6. **cursor-registry.ts** — Independent frame bus subscriber (parallel to `usePresence`, not delegated). Dispatches `setRemoteCursors()`/`setRemoteSelections()` as CodeMirror `StateEffect`s directly to registered `EditorView` instances — bypasses React entirely for low-latency cursor rendering.

### Mutation flow

Cell mutations (add, delete, edit) go through the WASM handle for instant response. Source edits are batched via `engine.scheduleFlush()` (20ms debounce), with `engine.flush()` before execute/save. The fast path for typing: `updateCellSource()` → WASM `update_source()` → `updateCellById()` (one cell, one subscriber) → debounced sync to daemon.

Execution requests go to the daemon via dedicated Tauri commands (`execute_cell_via_daemon`, etc.). These are slated to migrate onto `host.transport.sendRequest(NotebookRequest)` in a follow-up — for now they still `invoke(...)` directly.

### CellChangeset types

The `CellChangeset` shape originates in Rust (`notebook-doc/src/diff.rs`), but
the current TypeScript source of truth lives in `packages/runtimed/src/cell-changeset.ts`.
The notebook app re-exports those helpers through `apps/notebook/src/lib/cell-changeset.ts`
and `apps/notebook/src/lib/frame-pipeline.ts`:
- `CellChangeset` — `{ changed, added, removed, order_changed }`
- `ChangedCell` — `{ cell_id, fields }` where `fields` has boolean flags per field (`source`, `outputs`, `execution_count`, `cell_type`, `metadata`, `position`, `resolved_assets`)
- `mergeChangesets()` — union semantics for the coalescing window

## Key Files

| File | Role |
|------|------|
| `apps/notebook/tsconfig.json` | Path alias configuration |
| `apps/notebook/src/App.tsx` | Root component, provider setup |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM handle owner, `scheduleMaterialize`, `CellChangeset` dispatch |
| `apps/notebook/src/lib/materialize-cells.ts` | WASM → React conversion |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory pub/sub for broadcast and presence dispatch |
| `apps/notebook/src/hooks/usePresence.ts` | Remote presence tracking |
| `packages/runtimed/src/transport.ts` | Shared `FrameType` constants and transport interface |
| `apps/notebook/src/lib/frame-pipeline.ts` | App-side frame event processing and materialization planning |
| `src/components/outputs/media-router.tsx` | Output type dispatch |
| `src/components/editor/codemirror-editor.tsx` | Main editor |
