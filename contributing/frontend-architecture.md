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
│   ├── notebook/onboarding/      ← Onboarding sub-app (separate HTML entry point)
│   ├── notebook/settings/        ← Settings sub-app
│   ├── notebook/upgrade/         ← Upgrade sub-app
│   └── sidecar/                  ← Sidecar app
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
- It uses Tauri APIs (`@tauri-apps/api`)
- It interacts with the daemon (kernel execution, notebook sync)
- It's specific to the notebook editing experience

## Key Shared Components

### Cell Components (`src/components/cell/`)

| Component | Role |
|-----------|------|
| `CellContainer` | Wrapper with selection, focus, drag handles |
| `CellControls` | Play button, cell type indicator |
| `CellHeader` | Execution count display |
| `PlayButton` | Execute cell action |

### Editor (`src/components/editor/`)

CodeMirror integration with Jupyter-specific extensions:
- `codemirror-editor.tsx` — Main editor component
- `extensions.ts` — Keybindings, line numbers, bracket matching
- `languages.ts` — Python, Markdown, SQL syntax
- `themes.ts` — Light and dark themes

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
| `useAutomergeNotebook` | Owns WASM NotebookHandle, drives cell state |
| `useDaemonKernel` | Kernel execution, status broadcasts |
| `usePresence` | Remote cursor/selection tracking via presence frames |
| `useEnvProgress` | Environment setup progress tracking |
| `useDependencies` | UV dependency management |
| `useCondaDependencies` | Conda dependency management |
| `useDenoDependencies` | Deno dependency management |
| `useManifestResolver` | Resolves blob hashes to output data |
| `useCellKeyboardNavigation` | Arrow keys, enter/escape modes |
| `useEditorRegistry` | CodeMirror editor instance registry |
| `useGitInfo` | Git branch/status for the notebook file |
| `useGlobalFind` | Global find-and-replace across cells |
| `useHistorySearch` | Kernel input history search |
| `useTrust` | Notebook trust verification state |
| `useUpdater` | App update checking and installation |

## Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│ Frontend                                                        │
│                                                                 │
│  Tauri relay ── "notebook:frame" ──► useAutomergeNotebook       │
│                                      (WASM receive_frame demux) │
│                                        │          │         │   │
│                          sync_applied ─┘          │         │   │
│                          ▼                        │         │   │
│                   ┌──────────────┐                │         │   │
│                   │cellSnapshots-│  emitBroadcast  │ emitPresence│
│                   │ToNotebook-   │  (frame bus)    │ (frame bus) │
│                   │Cells()       │                 │             │
│                   └──────┬───────┘        │         │       │   │
│                          │                ▼         │       │   │
│                          │        ┌──────────────┐  │       │   │
│                          │        │useDaemonKernel│  │       │   │
│                          │        │useEnvProgress │  │       │   │
│                          │        └──────┬───────┘  │       │   │
│                          │               │          ▼       │   │
│                          │               │   ┌────────────┐ │   │
│                          │               │   │usePresence │ │   │
│                          │               │   └─────┬──────┘ │   │
│                          ▼               ▼         ▼        │   │
│  ┌─────────────────────────────────────────────────────────┐│   │
│  │ React Components                                         ││   │
│  │ (CellContainer → CodeCell/MarkdownCell → Outputs)        ││   │
│  └─────────────────────────────────────────────────────────┘│   │
└─────────────────────────────────────────────────────────────────┘
```

1. **useAutomergeNotebook** — Single ingress point. Listens for `notebook:frame`, demuxes via WASM `receive_frame()`, applies sync locally, dispatches to downstream hooks via the in-memory frame bus (`emitBroadcast()` and `emitPresence()` from `notebook-frame-bus.ts`)
2. **cellSnapshotsToNotebookCells()** / **cellSnapshotsToNotebookCellsSync()** — Converts WASM cell snapshots to React-friendly objects on sync changes
3. **useDaemonKernel / useEnvProgress** — Subscribe via `subscribeBroadcast()` from the frame bus for kernel status, outputs, and environment progress
4. **usePresence** — Subscribes via `subscribePresence()` from the frame bus for remote cursor/selection state

Cell mutations (add, delete, edit) go through the WASM handle for instant response, then sync to the daemon via `invoke("send_frame", { frameData })` where `frameData` includes the type byte prefix. Execution requests go to the daemon via dedicated Tauri commands.

## Key Files

| File | Role |
|------|------|
| `apps/notebook/tsconfig.json` | Path alias configuration |
| `apps/notebook/src/App.tsx` | Root component, provider setup |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM notebook sync |
| `apps/notebook/src/lib/materialize-cells.ts` | WASM → React conversion |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory pub/sub for broadcast and presence dispatch |
| `apps/notebook/src/hooks/usePresence.ts` | Remote presence tracking |
| `apps/notebook/src/lib/frame-types.ts` | Frame type constants (mirrors Rust) |
| `src/components/outputs/media-router.tsx` | Output type dispatch |
| `src/components/editor/codemirror-editor.tsx` | Main editor |
