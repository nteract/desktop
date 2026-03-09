# Documentation Gaps Analysis

Audit of what's missing from `contributing/` docs and `CLAUDE.md` relative to the actual codebase.

## High Priority (affects daily development)

### 1. Shared `src/` vs `apps/` code organization

No guide explains the split between top-level `src/` (shared components) and `apps/notebook/src/` (app-specific code). Developers don't know where new code should go.

**What exists undocumented in `src/`:**

- `src/components/cell/` — 15 cell components (CellContainer, CellControls, CellHeader, PlayButton, ExecutionCount, etc.)
- `src/components/editor/` — CodeMirror setup, extensions, themes, language support, search highlighting
- `src/components/isolated/` — iframe isolation implementation (IsolatedFrame, CommBridgeManager, frame-bridge, frame-html)
- `src/components/outputs/` — Output renderers (ANSI, HTML, image, JSON, markdown, SVG, media router)
- `src/components/widgets/` — Widget internals (anywidget-view, widget-store, widget-registry, ipycanvas, controls/)
- `src/bindings/` — TypeScript types generated from Rust via ts-rs (Runtime, SyncedSettings, ThemeMode, etc.)
- `src/hooks/` — useSyncedSettings, useTheme
- `src/isolated-renderer/` — Isolated renderer entry point, widget bridge client

**Suggested doc:** A `contributing/frontend-architecture.md` explaining the code organization, data flow, and where to put new components.

### 2. Frontend component architecture

No contributing guide covers how the major frontend subsystems work:

- **Cell rendering pipeline** — CellContainer → CodeCell/MarkdownCell → OutputArea
- **Editor system** — CodeMirror extensions, language registration, themes, keybindings (`src/components/editor/`)
- **Output rendering** — Media router pattern, output type dispatch (`src/components/outputs/`)
- **Keyboard navigation** — `useCellKeyboardNavigation.ts`
- **Tab completion** — `kernel-completion.ts`, `tab-completion.ts`
- **Global find** — `useGlobalFind.ts`, `GlobalFindBar.tsx`
- **History search** — `useHistorySearch.ts`, `HistorySearchDialog.tsx`
- **Manifest resolution** — `useManifestResolver.ts`, `materialize-cells.ts`

### 3. Unit and integration testing

`contributing/e2e.md` is thorough, but there's no guide for other test types:

- **Vitest frontend tests** — `vitest.config.ts` exists, `src/components/__tests__/` has tests, but no guide on running them or writing new ones
- **Rust unit tests** — No guide on `cargo test` patterns across crates
- **Hone tests** — `crates/runt/tests/` has 7 `.hone` test files using a custom framework with zero documentation
- **WASM tests** — `crates/runtimed-wasm/tests/` has cross-implementation and deno smoke tests
- **Python tests** — `python/runtimed/tests/` has 5 test files; only briefly mentioned in `docs/mcp-server.md`

**Suggested doc:** A `contributing/testing.md` covering all test types, how to run them, and when to use each.

### 4. TypeScript bindings generation

`src/bindings/` contains TypeScript types generated from Rust via `ts-rs` annotations. No documentation explains:

- How bindings are generated (which command, which crate drives it)
- When to regenerate (after changing Rust struct definitions)
- How `ts-rs` annotations work in the Rust crates
- Which types are generated vs hand-written

### 5. Widget development guide

`docs/widgets.md` covers user-facing widget support, but developers have no guide for:

- How to add support for a new widget type
- Widget store architecture (`widget-store.ts`, `widget-registry.ts`)
- Comm bridge protocol implementation details
- How anywidget ESM loading works
- How ipycanvas's custom implementation works (`src/components/widgets/ipycanvas/`)
- The `use-comm-router.ts` hook
- Widget controls implementation (`src/components/widgets/controls/`)

## Medium Priority (affects specific workflows)

### 6. Sidecar app

`apps/sidecar/` and `crates/sidecar/` have no coverage in `contributing/`:

- What sidecar is (output viewer for kernel outputs, embedded via `rust-embed`)
- How it relates to the main notebook app
- How to develop and modify it
- Its build pipeline (briefly mentioned in `contributing/build-dependencies.md` but not explained)
- How `@jupyter-widgets/html-manager` is used in the sidecar

### 7. Blob store and manifest system

The blob store is a core architectural component with no developer guide:

- How content-addressed storage works in practice
- The manifest/ContentRef system for large outputs
- How `useManifestResolver.ts` and `materialize-cells.ts` work together
- The inlining threshold decision
- How blob data flows: kernel → daemon `output_store.rs` → `blob_store.rs`/`blob_server.rs` → frontend

Key files: `crates/runtimed/src/blob_store.rs`, `blob_server.rs`, `output_store.rs`, `apps/notebook/src/lib/manifest-resolution.ts`, `materialize-cells.ts`

### 8. Onboarding and upgrade flows

Two separate mini-apps exist with no documentation:

- `apps/notebook/onboarding/` — Own App.tsx, main.tsx, types.ts
- `apps/notebook/upgrade/` — Own App.tsx, main.tsx, types.ts

No docs explain when they appear, how to modify them, or how they're routed to.

### 9. CI/CD pipeline internals

`RELEASING.md` covers release streams and artifacts, but `contributing/` lacks:

- How `build.yml` works (the main CI pipeline)
- How `pr-binary-generation.yml` creates PR binaries
- How to debug CI failures (`scripts/summarize-ci-log.py` exists but is undocumented)
- The Docker E2E pipeline (`docker-compose.yml`, `e2e/Dockerfile`)
- Code signing and notarization process (mentioned in RELEASING.md but not explained)

### 10. Python bindings development

`docs/python-bindings.md` and `contributing/runtimed.md` briefly cover `runtimed-py`, but missing:

- How to develop and modify the PyO3 bindings (`crates/runtimed-py/`)
- How maturin builds work
- How the MCP server (`python/runtimed/`) relates to the Rust crate
- Testing strategy for Python bindings
- The `_ipython_bridge`, `_sidecar`, and `_binary` modules in the Python package

## Lower Priority (niche or inferable)

### 11. Undocumented crates

| Crate | Gap |
|-------|-----|
| `runt-workspace` | Zero mentions in any doc. Provides workspace/dev-mode path utilities. |
| `tauri-jupyter` | Zero mentions in any doc. Shared Jupyter message types and base64 utilities for Tauri apps. |
| `kernel-env` | Minimal mention. UV and Conda environment creation with progress reporting. Unclear how it differs from `kernel-launch`. |
| `runt` (CLI) | Commands listed in `contributing/runtimed.md` but no guide on extending the CLI or its `kernel_client.rs`. |
| `notebook-doc` | Mentioned in CLAUDE.md key files but no guide on the Automerge schema or adding new document fields. |

### 12. Multi-window sync

CLAUDE.md mentions multi-window sync, but no guide covers:

- How multiple windows connect to the same notebook room
- The collaborator presence system (`CollaboratorAvatars.tsx`, `PresenceBookmarks.tsx`)
- Known limitations (e.g., #276 widget sync across windows)

### 13. Auto-updater

`useUpdater.ts` exists alongside `tauri-plugin-updater` dependency, but no documentation about:

- How auto-update works
- Update channels and distribution

### 14. Vite plugin for isolated renderer

`apps/notebook/vite-plugin-isolated-renderer.ts` is a custom Vite plugin with no documentation explaining what it does or why it's needed.

### 15. Development scripts

Undocumented utilities:

- `scripts/linux_cloud_dev.sh` — Linux cloud development setup
- `scripts/summarize-ci-log.py` — CI log summarization
- `bin/dev` — Development wrapper
- `bin/runt` — CLI wrapper
