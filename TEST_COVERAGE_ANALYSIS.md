# Test Coverage Analysis

## Executive Summary

This analysis examines the test coverage across the nteract/desktop codebase, covering Rust crates, TypeScript/React frontend, Python bindings, and E2E tests. While the codebase has a solid testing foundation (458 `#[test]` annotations across 43 Rust files, 10 Vitest unit test files, 9 E2E specs, and Python integration tests), there are significant coverage gaps — particularly in the frontend hooks/components, several critical Rust daemon modules, and cross-layer integration points. **No code coverage tooling is configured**, making it impossible to quantify coverage precisely.

---

## Current Test Inventory

| Layer | Framework | Test Files | Approx. Test Count |
|-------|-----------|-----------|-------------------|
| **Rust unit/inline** | `cargo test` | 43 files with `#[cfg(test)]` | ~458 `#[test]` annotations |
| **Rust integration** | `cargo test` | 2 (`runtimed/tests/integration.rs`, `notebook/tests/env_fallback.rs`) | ~24 |
| **TypeScript unit** | Vitest + jsdom | 10 test files | ~50 |
| **E2E** | WebdriverIO + Mocha | 9 spec files | ~20 |
| **Python** | Pytest + asyncio | 5 test files | ~30 |
| **WASM** | Deno test runner | 1 test directory | ~5 |

---

## Rust Crate Coverage Breakdown

### Well-Tested Crates

These crates have meaningful inline test modules:

| Crate/Module | Test Count | What's Covered |
|-------------|-----------|----------------|
| `notebook-doc/src/lib.rs` | 32 | Core Automerge doc operations, cell mutations, metadata |
| `runtimed/src/notebook_sync_server.rs` | 43 | Environment detection, kernel auto-launch, sync logic |
| `runtimed/src/settings_doc.rs` | 38 | Settings CRDT document operations |
| `notebook/src/environment_yml.rs` | 42 | environment.yml parsing and discovery |
| `runtimed/src/protocol.rs` | 26 | Jupyter protocol message parsing |
| `runtimed/src/output_store.rs` | 16 | Output blob storage and retrieval |
| `notebook/src/pyproject.rs` | 14 | pyproject.toml parsing |
| `runtimed/src/daemon.rs` | 18 | Daemon lifecycle, pool management |
| `runtimed/src/connection.rs` | 14 | ZMQ connection management |
| `runtimed/src/stream_terminal.rs` | 12 | Terminal stream handling |
| `runtimed/src/blob_store.rs` | 12 | Content-addressed blob storage |
| `runtimed/src/comm_state.rs` | 12 | Widget comm state machine |
| `notebook/src/deno_env.rs` | 12 | Deno environment detection |
| `notebook/src/pixi.rs` | 11 | pixi.toml parsing |
| `notebook/src/project_file.rs` | 11 | Closest-wins project file detection |
| `notebook/src/settings.rs` | 9 | Settings schema and defaults |

### Under-Tested Crates (Have Tests but Gaps)

| Crate/Module | Test Count | Key Gaps |
|-------------|-----------|----------|
| `kernel-launch/src/tools.rs` | 8 | Only tests tool path resolution; no tests for bootstrap error handling or concurrent bootstrapping |
| `notebook/src/typosquat.rs` | 6 | Limited coverage of edge cases in package name similarity detection |
| `runt-trust/src/lib.rs` | 6 | Basic HMAC signing/verification only; no tests for key rotation, corrupt key file handling |
| `kernel-env/src/uv.rs` | 5 | Basic UV env creation; no error path testing |
| `kernel-env/src/conda.rs` | 5 | Basic conda env creation; no error path testing |
| `runtimed/src/blob_server.rs` | 5 | Basic HTTP blob serving; no range request or concurrent access tests |
| `runtimed/src/runtime.rs` | 5 | Runtime detection basics only |
| `runtimed/src/service.rs` | 4 | Service lifecycle basics |
| `runtimed/src/project_file.rs` | 4 | Project file detection for daemon side |
| `notebook/src/shell_env.rs` | 4 | Shell environment variable capture |
| `runtimed/src/kernel_manager.rs` | 8 | Kernel process spawn/lifecycle, but no crash recovery or resource cleanup tests |
| `notebook/src/uv_env.rs` | 3 | Minimal; no cache invalidation or concurrent creation tests |
| `notebook/src/conda_env.rs` | 5 | Minimal; no timeout or partial install recovery tests |
| `notebook/src/menu.rs` | 7 | Menu item creation but no action handler tests |

### Untested Rust Source Files (No `#[cfg(test)]` Module)

These files have **zero** inline unit tests:

| File | Risk Level | What It Does |
|------|-----------|--------------|
| `runtimed/src/inline_env.rs` | **HIGH** | Cached environment creation for inline deps — critical path for kernel launch |
| `runtimed/src/sync_server.rs` | **HIGH** | Automerge sync server — core of the local-first architecture |
| `runtimed/src/lib.rs` | Medium | Re-exports and daemon public API surface |
| `runtimed/src/main.rs` | Low | CLI entry point |
| `runtimed/src/terminal_size.rs` | Low | Terminal size detection |
| `runtimed-wasm/src/lib.rs` | **HIGH** | WASM bindings for NotebookDoc — sole interface between frontend and Automerge |
| `runtimed-py/src/*.rs` (all 10 files) | **HIGH** | Python bindings — entire PyO3 layer is untested in Rust |
| `notebook/src/trust.rs` | **HIGH** | HMAC trust verification — security-critical |
| `notebook/src/runtime.rs` | Medium | Runtime type detection |
| `notebook/src/session.rs` | Medium | Session management |
| `notebook/src/cli_install.rs` | Low | CLI installation logic |
| `notebook/src/webdriver.rs` | Low | WebDriver test support |
| `kernel-launch/src/lib.rs` | **HIGH** | Shared kernel launching API — the entry point for all kernel spawning |
| `kernel-env/src/progress.rs` | Low | Progress reporting |
| `kernel-env/src/lib.rs` | Low | Re-exports |
| `sidecar/src/lib.rs` | Medium | Sidecar process management (has 2 tests, but minimal) |
| `runt/src/kernel_client.rs` | Medium | CLI kernel client |
| `tauri-jupyter/src/lib.rs` | Low | Re-exports |

---

## TypeScript/React Coverage Breakdown

### Existing Tests (10 files in `src/`)

All existing Vitest tests live under `src/` (the shared elements package), none under `apps/notebook/src/`:

| Test File | What It Tests |
|-----------|--------------|
| `src/lib/__tests__/ErrorBoundary.test.tsx` | Error boundary component |
| `src/components/widgets/__tests__/output-widget.test.tsx` | Output widget rendering |
| `src/components/widgets/__tests__/widget-store.test.ts` | Widget state store |
| `src/components/widgets/__tests__/buffer-utils.test.ts` | Binary buffer utilities |
| `src/components/isolated/__tests__/frame-html.test.ts` | Isolated frame HTML generation |
| `src/components/isolated/__tests__/comm-bridge-manager.test.ts` | Comm bridge lifecycle |
| `src/components/isolated/__tests__/isolated-frame.test.ts` | Iframe isolation |
| `src/components/isolated/__tests__/frame-bridge.test.ts` | Frame bridge communication |
| `src/components/outputs/__tests__/ansi-output.test.tsx` | ANSI terminal output rendering |
| `src/components/outputs/__tests__/media-router.test.tsx` | MIME-type output routing |

### Untested Frontend Code — `apps/notebook/src/` (ZERO tests)

The entire notebook application has **no unit tests**. Every file below is untested:

#### Critical Hooks (0/13 tested)
| Hook | Risk Level | Why It Needs Tests |
|------|-----------|-------------------|
| `useAutomergeNotebook.ts` | **CRITICAL** | Core notebook state management via WASM; drives all cell operations |
| `useDaemonKernel.ts` | **CRITICAL** | Kernel lifecycle, execution, status — the primary compute interface |
| `useDependencies.ts` | **HIGH** | UV dependency management, trust validation |
| `useCondaDependencies.ts` | **HIGH** | Conda dependency management |
| `useDenoDependencies.ts` | **HIGH** | Deno dependency management |
| `useTrust.ts` | **HIGH** | Security-critical trust decisions |
| `useManifestResolver.ts` | Medium | Blob manifest resolution for outputs |
| `useEnvProgress.ts` | Medium | Environment creation progress tracking |
| `useCellKeyboardNavigation.ts` | Medium | Keyboard navigation between cells |
| `useGlobalFind.ts` | Medium | Find/replace across notebook |
| `useHistorySearch.ts` | Medium | Command history search |
| `useGitInfo.ts` | Low | Git branch display |
| `useUpdater.ts` | Low | App update checking |

#### Critical Utilities (0/8 tested)
| Utility | Risk Level | Why It Needs Tests |
|---------|-----------|-------------------|
| `lib/materialize-cells.ts` | **CRITICAL** | Converts WASM cell snapshots to React state — data integrity |
| `lib/kernel-completion.ts` | **HIGH** | Tab completion logic |
| `lib/tab-completion.ts` | **HIGH** | Tab completion UI logic |
| `lib/notebook-metadata.ts` | **HIGH** | Notebook metadata parsing and manipulation |
| `lib/notebook-cells.ts` | **HIGH** | Cell ordering and operations |
| `lib/kernel-status.ts` | Medium | Kernel status state machine |
| `lib/manifest-resolution.ts` | Medium | Output manifest resolution |
| `lib/open-url.ts` | Low | URL opening utility |

#### Components (0/15 tested)
| Component | Risk Level |
|-----------|-----------|
| `App.tsx` | Medium |
| `NotebookView.tsx` | **HIGH** — main notebook renderer |
| `CodeCell.tsx` | **HIGH** — code editing and execution |
| `MarkdownCell.tsx` | Medium |
| `NotebookToolbar.tsx` | Medium |
| `TrustDialog.tsx` | **HIGH** — security UI |
| `DependencyHeader.tsx` | Medium |
| `CondaDependencyHeader.tsx` | Medium |
| `UntrustedBanner.tsx` | Medium |
| `DaemonStatusBanner.tsx` | Medium |
| `GlobalFindBar.tsx` | Medium |
| `HistorySearchDialog.tsx` | Low |
| `DebugBanner.tsx` | Low |
| `CellSkeleton.tsx` | Low |
| `icons.tsx` | Low |

### Untested `src/` Code

Beyond the 10 tested files, these `src/` modules lack tests:

| Module | Risk Level |
|--------|-----------|
| `src/components/cell/*` (12 files) | **HIGH** — Cell UI components (OutputArea, CellHeader, PlayButton, etc.) |
| `src/components/widgets/controls/*` (~30 widgets) | Medium — Individual ipywidget implementations |
| `src/components/widgets/widget-registry.ts` | **HIGH** — Widget type resolution |
| `src/components/widgets/anywidget-view.tsx` | Medium — Anywidget rendering |
| `src/isolated-renderer/*` | Medium — Isolated iframe renderer |
| `src/hooks/useSyncedSettings.ts` | Medium — Settings sync |
| `src/hooks/useTheme.ts` | Low — Theme management |
| `src/lib/dark-mode.ts` | Low |
| `src/lib/highlight-text.ts` | Low |

---

## E2E Test Gaps

### Current E2E Specs (9 total)
- `smoke.spec.js` — Basic cell execution
- `tab-completion.spec.js` — Tab completion
- `prewarmed-uv.spec.js` — UV prewarmed environment
- `deno.spec.js` — Deno kernel
- `uv-inline.spec.js` — UV inline dependencies
- `conda-inline.spec.js` — Conda inline dependencies
- `uv-pyproject.spec.js` — pyproject.toml detection
- `untitled-pyproject.spec.js` — Untitled notebook in pyproject directory
- `trust-dialog-dismiss.spec.js` — Trust dialog behavior

### Missing E2E Scenarios
| Scenario | Risk Level | Description |
|----------|-----------|-------------|
| **Pixi project file detection** | **HIGH** | No E2E test for `pixi.toml` environments |
| **environment.yml detection** | **HIGH** | No E2E test for conda `environment.yml` |
| **Multiple cells and ordering** | **HIGH** | No test for cell reordering, deletion, insertion |
| **Markdown cell editing** | Medium | No test for markdown cell create/edit/render |
| **Kernel restart/interrupt** | **HIGH** | No test for kernel interrupt or restart flows |
| **Find/Replace** | Medium | No test for global find/replace |
| **Keyboard navigation** | Medium | No test for cell-to-cell keyboard navigation |
| **Large output handling** | Medium | No test for large/streaming outputs |
| **Error output rendering** | Medium | No test for traceback/error display |
| **Widget rendering** | Medium | No test for ipywidget display and interaction |
| **Multi-notebook** | Low | No test for multiple notebooks open |
| **Notebook save/load** | **HIGH** | No test for save persistence and reload |

---

## Python Bindings (`runtimed-py`) Coverage

### Current Tests
- `test_session_unit.py` — Unit tests for session management
- `test_daemon_integration.py` — Daemon integration (Linux CI only)
- `test_ipython_bridge.py` — IPython bridge
- `test_binary.py` — Binary loading
- `test_sidecar.py` — Sidecar process

### Gaps
- **No Rust-side tests** — All 10 `runtimed-py/src/*.rs` files have zero `#[test]` modules
- **No macOS/Windows CI** — Integration tests only run on Linux
- **No output resolution tests** — `output_resolver.rs` is untested
- **No event stream tests** — `event_stream.rs` is untested

---

## Infrastructure Gaps

### No Code Coverage Tooling
- No Rust coverage tool (tarpaulin, llvm-cov, cargo-llvm-cov)
- No JS/TS coverage tool (vitest `--coverage`, c8, istanbul)
- No coverage CI gates or reporting
- No coverage badges or trend tracking

### CI Limitations
- Windows E2E testing disabled for PRs (`run_on_pr: false`)
- Python integration tests Linux-only
- No performance benchmarks or regression detection
- WASM tests are minimal (Deno runner only)

---

## Recommended Priority Improvements

### Priority 1 — High Impact, Security/Correctness Critical

1. **`apps/notebook/src/lib/materialize-cells.ts`** — Unit tests for cell materialization. This is the bridge between WASM CRDT state and React; bugs here silently corrupt the UI.

2. **`crates/notebook/src/trust.rs`** — Unit tests for HMAC trust verification. Security-critical module with zero tests.

3. **`crates/runtimed/src/inline_env.rs`** — Unit tests for cached inline environment creation. Cache key collisions or stale cache bugs affect kernel launches.

4. **`apps/notebook/src/hooks/useAutomergeNotebook.ts`** — Unit tests (or targeted integration tests) for the core notebook state hook. Mock the WASM handle and test cell CRUD operations, sync message flow.

5. **`crates/kernel-launch/src/lib.rs`** — Unit tests for the shared kernel launching API. Every kernel spawn flows through this module.

### Priority 2 — High Impact, Functional Correctness

6. **`apps/notebook/src/hooks/useDaemonKernel.ts`** — Test kernel lifecycle states, execution request/response flow, and error handling.

7. **`apps/notebook/src/lib/notebook-cells.ts` and `notebook-metadata.ts`** — Test cell ordering logic and metadata parsing.

8. **E2E: Kernel restart/interrupt** — Add an E2E spec that interrupts a long-running cell and verifies the kernel recovers.

9. **E2E: Cell operations** — Add E2E tests for add/delete/reorder cells, verifying persistence.

10. **E2E: Pixi and environment.yml** — Add fixture tests for the remaining environment detection backends.

### Priority 3 — Coverage Infrastructure

11. **Add `cargo-llvm-cov`** to CI — Generate Rust coverage reports and set a baseline.

12. **Add `vitest --coverage`** to CI — Enable `@vitest/coverage-v8` and set a baseline for TypeScript.

13. **Add coverage CI gate** — Fail PRs that decrease coverage below a threshold.

### Priority 4 — Medium Priority Gaps

14. **`runtimed-wasm/src/lib.rs`** — Integration tests that exercise the WASM bindings from JS (beyond current minimal Deno tests).

15. **`runtimed-py` Rust-side tests** — Add `#[test]` modules for PyO3 binding logic, especially `output_resolver.rs` and `event_stream.rs`.

16. **`src/components/cell/*`** — Unit tests for cell UI components (OutputArea, PlayButton, ExecutionStatus).

17. **`apps/notebook/src/hooks/useDependencies.ts` and `useCondaDependencies.ts`** — Test dependency add/remove/trust flows.

18. **E2E: Notebook save/load** — Verify that notebook modifications persist across app restart.

---

## Summary Metrics

| Area | Source Files | Files with Tests | Estimated Coverage |
|------|-------------|-----------------|-------------------|
| Rust crates | ~70 | 43 (61%) have `#[cfg(test)]` | ~40-60% line coverage (estimated) |
| `apps/notebook/src/` | ~44 | 0 (0%) | **0%** |
| `src/` (elements) | ~90+ | 10 (~11%) have tests | ~10-15% line coverage |
| E2E | N/A | 9 specs | Covers happy-path basics only |
| Python | ~10 | 5 | ~40-50% (unit only) |

**The single largest gap is the `apps/notebook/src/` directory — the entire notebook application — which has zero unit tests.** Prioritizing `materialize-cells.ts`, `useAutomergeNotebook.ts`, and `useDaemonKernel.ts` would provide the highest return on testing investment.
