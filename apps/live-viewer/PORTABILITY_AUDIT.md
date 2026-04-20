# Component Portability Audit

Assessment of what blocks reuse of nteract notebook app UI components in non-Tauri contexts (e.g., this browser-served live viewer).

## Summary

**Only 10 files out of 100+ have `@tauri-apps` imports.** The architecture is already designed for portability via the `NotebookHost` abstraction.

---

## Shared Components (`src/components/`) — ALL REUSABLE AS-IS

Zero Tauri or app-specific imports. The live viewer imports these via the `@/` path alias:

| Component | Path | Status |
|-----------|------|--------|
| `CellContainer` | `src/components/cell/CellContainer.tsx` | Used by live-viewer |
| `ExecutionCount` | `src/components/cell/ExecutionCount.tsx` | Used by live-viewer |
| `ExecutionStatus` | `src/components/cell/ExecutionStatus.tsx` | Used by live-viewer |
| `OutputArea` | `src/components/cell/OutputArea.tsx` | Available (needs WidgetStoreProvider) |
| `gutter-colors` | `src/components/cell/gutter-colors.ts` | Used by live-viewer |
| `MediaRouter` | `src/components/outputs/media-router.tsx` | Used by live-viewer |
| `AnsiStreamOutput` | `src/components/outputs/ansi-output.tsx` | Used by live-viewer |
| `AnsiErrorOutput` | `src/components/outputs/ansi-output.tsx` | Used by live-viewer |
| `CodeMirrorEditor` | `src/components/editor/codemirror-editor.tsx` | Used by live-viewer |
| All output renderers | `src/components/outputs/*.tsx` | Available |
| All UI primitives | `src/components/ui/*.tsx` | Available (shadcn) |

---

## Files with `@tauri-apps` Imports (Exhaustive)

| # | File | Import | Purpose | Viewer needs it? |
|---|------|--------|---------|-----------------|
| 1 | `apps/notebook/src/lib/tauri-rx.ts` | `getCurrentWebview` | Frame events as RxJS Observable | No — SyncEngine handles frames |
| 2 | `apps/notebook/src/hooks/useAutomergeNotebook.ts` | `fromTauriEvent` + Host | Wires WASM handle to Tauri transport | No — viewer uses WebSocketTransport |
| 3 | `apps/notebook/src/hooks/useDaemonKernel.ts` | `invoke` + `getCurrentWebview` | Kernel execution commands | No — read-only viewer |
| 4 | `apps/notebook/src/lib/notebook-file-ops.ts` | `invoke` (9x) | Save/open/clone notebooks | No — read-only |
| 5 | `apps/notebook/src/hooks/useDependencies.ts` | `invoke` | UV dependency detection | No |
| 6 | `apps/notebook/src/hooks/useCondaDependencies.ts` | `invoke` (3x) | Conda environment detection | No |
| 7 | `apps/notebook/src/hooks/usePixiDependencies.ts` | `invoke` | Pixi project detection | No |
| 8 | `apps/notebook/src/hooks/useDenoDependencies.ts` | `invoke` | Deno config detection | No |
| 9 | `apps/notebook/src/hooks/useUpdater.ts` | `invoke` | App auto-update | No |
| 10 | `apps/notebook/src/components/PoolErrorBanner.tsx` | `invoke` | Settings window button | No |

---

## App Components (`apps/notebook/src/components/`)

| Component | Tauri-free? | Blocker |
|-----------|-------------|---------|
| `NotebookView.tsx` | Yes | Uses app stores (`notebook-cells`, `cell-ui-state`, `PresenceContext`) — all pure React |
| `CodeCell.tsx` | Yes | Uses `useCrdtBridge` (clean), `kernel-completion` (takes Host type), `open-url` (takes Host) |
| `MarkdownCell.tsx` | Yes | Same pattern as CodeCell |
| `RawCell.tsx` | Yes | Same |
| `PoolErrorBanner.tsx` | **No** | Direct `invoke("open_settings_window")` |

---

## App Libraries

| Module | Tauri-free? | Notes |
|--------|-------------|-------|
| `notebook-cells.ts` | Yes | Pure `useSyncExternalStore` |
| `runtime-state.ts` | Yes | Re-exports from `runtimed` package |
| `notebook-frame-bus.ts` | Yes | Pure in-memory pub/sub |
| `materialize-cells.ts` | Yes | WASM → React conversion |
| `cell-ui-state.ts` | Yes | Pure store |
| `tauri-rx.ts` | **No** | `getCurrentWebview()` — 42 lines, not needed |
| `notebook-file-ops.ts` | **No** | 9x invoke — not needed for viewer |
| `kernel-completion.ts` | Partially | Takes `NotebookHost` type param |

---

## Decoupling Difficulty

| Level | What | Fix |
|-------|------|-----|
| **Done** | Frame transport | Live viewer uses `WebSocketTransport` → `SyncEngine` |
| **Done** | Shared components | `CellContainer`, `ExecutionCount`, `MediaRouter`, `CodeMirrorEditor` all imported |
| **~200 LOC** | `createBrowserHost()` | Implement `NotebookHost` with WebSocket transport + no-ops |
| **Medium (6 files)** | Direct `invoke()` calls | Migrate to `host.transport.sendRequest()` |
| **Not needed** | File ops, deps, updater | Read-only viewer doesn't use these |

---

## Migration Roadmap

### Phase 1 (Complete)
- SyncEngine + WebSocketTransport wired directly
- RuntimeStateDoc subscription (kernel status, execution state, queue)
- Basic cell rendering with `gutter-colors` and `AnsiOutput`

### Phase 2 (Complete)
- `CellContainer` for full layout (ribbon, gutter, segmented code/output)
- `ExecutionCount` and `ExecutionStatus` for execution indicators
- `CodeMirrorEditor` (read-only) for syntax-highlighted source
- `MediaRouter` for rich output dispatch (images, JSON, markdown, LaTeX, etc.)
- `AnsiStreamOutput` / `AnsiErrorOutput` for stream/error outputs

### Phase 3 (Next — `createBrowserHost()`)
- Create `packages/notebook-host/src/browser/index.ts`
- Implement `NotebookHost` interface: transport = WebSocketTransport, blobs = relay endpoint, log = console, everything else = no-op
- Wrap app in `NotebookHostProvider` — unlocks the full `NotebookView` → `CodeCell` component tree
- Enables: presence, CRDT bridge (collaborative editing), kernel completion

### Phase 4 (Future — full editor parity)
- Migrate the 10 invoke-coupled files onto `host.transport.sendRequest()`
- Already planned in the frontend-architecture roadmap
- Enables: save, execute, dependency management through browser

---

## Architecture Layers

```
┌─────────────────────────────────────────────────────────────┐
│  FULLY PORTABLE (no Tauri deps)                              │
│                                                               │
│  src/components/cell/*      ← CellContainer, OutputArea      │
│  src/components/outputs/*   ← MediaRouter, all renderers     │
│  src/components/editor/*    ← CodeMirrorEditor               │
│  src/components/ui/*        ← shadcn primitives              │
│  packages/runtimed/         ← SyncEngine, RuntimeState       │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  HOST-ABSTRACTED (swappable via NotebookHost interface)       │
│                                                               │
│  apps/notebook/src/App.tsx             ← top-level wiring    │
│  apps/notebook/src/hooks/usePresence   ← host.transport      │
│  apps/notebook/src/hooks/useTrust      ← host.trust          │
│  apps/notebook/src/lib/blob-port.ts    ← host.blobs          │
│  apps/notebook/src/lib/logger.ts       ← host.log            │
│  apps/notebook/src/lib/open-url.ts     ← host.externalLinks  │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  TAURI-COUPLED (10 files, direct invoke/webview)             │
│                                                               │
│  tauri-rx.ts, useDaemonKernel.ts, notebook-file-ops.ts       │
│  useDependencies.ts, useCondaDependencies.ts                 │
│  usePixiDependencies.ts, useDenoDependencies.ts              │
│  useUpdater.ts, kernel-completion.ts, PoolErrorBanner.tsx    │
└─────────────────────────────────────────────────────────────┘
```
