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
│   │   ├── isolated/             ← Iframe security isolation (IsolatedFrame, CommBridge)
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
│   │   ├── hooks/                ← Notebook-specific hooks (useDaemonKernel, etc.)
│   │   ├── lib/                  ← App-specific utilities (materialize-cells.ts)
│   │   ├── wasm/                 ← WASM bindings (runtimed-wasm)
│   │   ├── App.tsx               ← Root component
│   │   └── types.ts              ← App types
│   │
│   └── sidecar/src/              ← Standalone output viewer for REPL use
│                                   (embeds via rust-embed in crates/sidecar/)
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
- It could be reused by sidecar or future apps
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
- `extensions/` — Keybindings, line numbers, bracket matching
- `languages/` — Python, Markdown, SQL syntax
- `themes/` — Light and dark themes

### Outputs (`src/components/outputs/`)

| Renderer | MIME Types |
|----------|------------|
| `AnsiOutput` | `text/plain` with ANSI codes |
| `HtmlOutput` | `text/html` |
| `ImageOutput` | `image/png`, `image/jpeg`, etc. |
| `MarkdownOutput` | `text/markdown` |
| `JsonOutput` | `application/json` |
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
| `useDependencies` | UV dependency management |
| `useCondaDependencies` | Conda dependency management |
| `useManifestResolver` | Resolves blob hashes to output data |
| `useCellKeyboardNavigation` | Arrow keys, enter/escape modes |

## Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│ Frontend                                                        │
│                                                                 │
│  ┌──────────────┐    sync     ┌──────────────────┐             │
│  │ NotebookHandle│◄──────────►│ Daemon           │             │
│  │ (WASM)       │             │ (notebook room)  │             │
│  └──────┬───────┘             └────────┬─────────┘             │
│         │                              │                        │
│         │ get_cells_json()             │ kernel broadcasts      │
│         ▼                              ▼                        │
│  ┌──────────────┐             ┌──────────────────┐             │
│  │ materialize- │             │ useDaemonKernel  │             │
│  │ Cells()      │             │                  │             │
│  └──────┬───────┘             └────────┬─────────┘             │
│         │                              │                        │
│         │ NotebookCell[]               │ outputs, status        │
│         ▼                              ▼                        │
│  ┌─────────────────────────────────────────────────┐           │
│  │ React Components                                 │           │
│  │ (CellContainer → CodeCell/MarkdownCell → Outputs)│           │
│  └─────────────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────────────┘
```

1. **NotebookHandle (WASM)** — Local Automerge doc for instant cell edits
2. **materializeCells()** — Converts WASM cell snapshots to React-friendly objects
3. **useDaemonKernel** — Receives kernel outputs and status via daemon broadcasts
4. **React components** — Render cells and outputs

Cell mutations (add, delete, edit) go through the WASM handle for instant response, then sync to the daemon. Execution requests go to the daemon, which reads from the synced document.

## Key Files

| File | Role |
|------|------|
| `apps/notebook/tsconfig.json` | Path alias configuration |
| `apps/notebook/src/App.tsx` | Root component, provider setup |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM notebook sync |
| `apps/notebook/src/lib/materialize-cells.ts` | WASM → React conversion |
| `src/components/outputs/media-router.tsx` | Output type dispatch |
| `src/components/editor/codemirror-editor.tsx` | Main editor |
