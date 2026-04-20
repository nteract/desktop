# Component Tree Integration Report

Full component tree swap attempted for the live-viewer.
Documents every coupling boundary between the live-viewer and the notebook app.

## What Works (shared components successfully imported)

| Component | Path | Notes |
|-----------|------|-------|
| `CellContainer` | `src/components/cell/CellContainer.tsx` | Zero coupling. Fully portable. |
| `CompactExecutionButton` | `src/components/cell/CompactExecutionButton.tsx` | Zero coupling. Fully portable. |
| `OutputArea` | `src/components/cell/OutputArea.tsx` | Works with WidgetStoreProvider context + IsolatedRendererProvider. |
| `IsolatedFrame` | `src/components/isolated/isolated-frame.tsx` | Works with renderer plugin stubs/artifacts. |
| `MediaRouter` | `src/components/outputs/media-router.tsx` | Fully portable for in-DOM outputs. |
| `AnsiStreamOutput` / `AnsiErrorOutput` | `src/components/outputs/ansi-output.tsx` | Zero coupling. |
| `CodeMirrorEditor` | `src/components/editor/codemirror-editor.tsx` | Works in read-only mode. |
| `WidgetStoreProvider` | `src/components/widgets/widget-store-context.tsx` | Requires crypto.randomUUID (HTTPS or polyfill). |
| `IsolatedRendererProvider` | `src/components/isolated/isolated-renderer-context.tsx` | Needs virtual module or pre-built artifacts. |
| `NotebookHostProvider` | `packages/notebook-host/` | Works with browser host implementation. |

## Coupling Boundaries (what cannot be imported cleanly)

### 1. Virtual Renderer Plugin Modules

**Edge:** `src/components/isolated/iframe-libraries.ts` imports `virtual:renderer-plugin/*`

**Problem:** These are Vite virtual modules provided by `apps/notebook/vite-plugin-isolated-renderer.ts`.
The plugin reads pre-built CJS artifacts from `apps/notebook/src/renderer-plugins/`.

**Solution:** Live-viewer has its own Vite plugin that reads the same pre-built artifacts.
Empty stubs work if you only need in-DOM rendering (isolated={false}).

**Affected MIME types:** text/markdown, text/latex, application/vnd.plotly.v1+json,
application/geo+json, application/vnd.vega.v5+json, application/vnd.apache.parquet

### 2. crypto.randomUUID (Secure Context)

**Edge:** `src/components/widgets/use-comm-router.ts` line 124

**Problem:** `const SESSION_ID = crypto.randomUUID()` executes at module load time.
`crypto.randomUUID` requires SecureContext (HTTPS or localhost).

**Solution:** Serve over HTTPS (Tailscale TLS certs) + polyfill for dev.

### 3. Cell UI State Store (notebook-cells.ts, cell-ui-state.ts)

**Edge:** `apps/notebook/src/lib/notebook-cells.ts` and `cell-ui-state.ts`

**Problem:** NotebookView, CodeCell, MarkdownCell all depend on these stores for:
- `useCell(id)` — per-cell reactive subscriptions
- `useIsCellFocused(id)` — focus state
- `useIsCellExecuting(id)` — execution state
- `useIsCellQueued(id)` — queue state
- `useCellIds()` — ordered cell list

**Impact:** Cannot use the app's `NotebookView` or `CodeCell` directly.

**Live-viewer approach:** Props-based rendering. The viewer owns its own cell array
via `useState<CellData[]>` and passes execution state as props to `CellView`.

### 4. CRDT Bridge (useCrdtBridge)

**Edge:** `apps/notebook/src/hooks/useCrdtBridge.ts`

**Problem:** CodeCell requires useCrdtBridge for character-level sync between
CodeMirror and the Automerge CRDT. This hook depends on the notebook's WASM
NotebookHandle and the global notebook-frame-bus.

**Impact:** Cannot use the app's CodeCell for editing.

**Live-viewer approach:** Read-only `CodeMirrorEditor` with no bridge.

### 5. Keyboard Navigation (useCellKeyboardNavigation)

**Edge:** `apps/notebook/src/hooks/useCellKeyboardNavigation.ts`

**Problem:** Provides Shift-Enter, Alt-Up/Down, etc. keybindings.
Depends on cell-ui-state and editor-registry.

**Impact:** Not needed for read-only viewer.

### 6. Presence System (PresenceContext, cursor-registry)

**Edge:** `apps/notebook/src/contexts/PresenceContext.tsx`

**Problem:** Renders remote peer cursors and selections. Requires:
- notebook-frame-bus presence subscription
- cursor-registry for per-editor cursor state
- EditorView instances (from useCrdtBridge)

**Impact:** Could be added later if we want to show who's viewing.

### 7. Editor Registry (useEditorRegistry)

**Edge:** `apps/notebook/src/hooks/useEditorRegistry.ts`

**Problem:** Global registry of CodeMirror EditorView instances.
Used by cell navigation and global find.

**Impact:** Not needed for read-only.

### 8. Drag-and-Drop (dnd-kit)

**Edge:** `NotebookView` uses `@dnd-kit/core` and `@dnd-kit/sortable`

**Problem:** Cell reordering via drag handles. Requires sortable context,
stable DOM order hack, and CRDT move_cell mutations.

**Impact:** Not needed for read-only viewer.

### 9. MarkdownCell with Edit/Preview Toggle

**Edge:** `apps/notebook/src/components/MarkdownCell.tsx`

**Problem:** Full markdown cell has:
- Click-to-edit with CRDT bridge
- Preview rendering via IsolatedFrame
- Presence sender extension
- Blob port for embedded images (useBlobPort)

**Live-viewer approach:** Render markdown source as `text/markdown` output
through OutputArea's iframe isolation (same visual result, no editing).

### 10. Blob Port Resolution

**Edge:** `apps/notebook/src/lib/blob-port.ts`

**Problem:** Resolves `attachment:` refs and blob hashes to URLs.
Needed for embedded images in markdown cells and widget ESM.

**Impact:** Limited — most outputs work without blob resolution.
Could be added by proxying daemon blob store through the relay server.

## Architecture Summary

```
┌─────────────────────────────────────────────────┐
│                  Live Viewer                      │
├─────────────────────────────────────────────────┤
│  NotebookHostProvider (browser host)             │
│    └── IsolatedRendererProvider                  │
│          └── WidgetStoreProvider                 │
│                └── NotebookViewer                │
│                      ├── CellView (code)        │
│                      │   ├── CompactExecButton  │
│                      │   ├── CodeMirrorEditor   │  ← read-only
│                      │   └── OutputArea         │
│                      │       ├── IsolatedFrame  │  ← rich outputs
│                      │       ├── MediaRouter    │  ← in-DOM outputs
│                      │       └── AnsiOutput     │
│                      └── CellView (markdown)    │
│                          └── OutputArea         │  ← renders as text/markdown
└─────────────────────────────────────────────────┘
         │
         │ WebSocket (Automerge sync + RuntimeState)
         │
┌────────▼────────────────────────────────────────┐
│            Relay Server (Rust/axum)               │
│  TLS (Tailscale certs) + SPA fallback            │
├─────────────────────────────────────────────────┤
│         Unix socket                              │
│            └── runtimed daemon                   │
└─────────────────────────────────────────────────┘
```

## What's Possible Next

1. **Widget rendering** — WidgetStoreProvider is already wired. Need to feed
   comm state from RuntimeStateDoc into the store (same as useDaemonKernel does).

2. **Blob port proxy** — Add `/blob/{hash}` route to relay server that proxies
   the daemon's blob store. Enables embedded images and widget ESM.

3. **Presence display** — Show colored dots for active viewers. Would need
   presence frame forwarding in the relay.

4. **Read-only NotebookView** — Factor out a `ReadOnlyNotebookView` from the
   app's `NotebookView` that uses the same stable-DOM-order trick but skips
   dnd-kit, cell-ui-state, and keyboard navigation.
