---
name: testing
description: Run and write tests. Use when running tests, writing new tests, debugging test failures, or working with test infrastructure.
---

# Testing

## Quick Reference

| Type | Location | Command | Framework |
|------|----------|---------|-----------|
| E2E | `e2e/specs/` | `cargo xtask e2e test` | WebdriverIO + Mocha |
| Frontend unit | `src/**/__tests__/`, `apps/notebook/src/**/__tests__/` | `pnpm test` | Vitest + jsdom |
| Rust unit | inline `#[cfg(test)]` | `cargo test` | built-in |
| CLI behavior | `crates/runt/tests/*.hone` | `cargo hone test` | Hone (not yet published) |
| Python | `python/runtimed/tests/` | `pytest` | pytest |

## Frontend Unit Tests (Vitest)

Config: `vitest.config.ts` (jsdom environment, globals enabled, setup in `./src/test-setup.ts`).

**Running:**

```bash
pnpm test         # Watch mode
pnpm test:run     # Run once
```

**Test locations:**

- `src/components/isolated/__tests__/` — Frame bridge, message protocol
- `src/components/outputs/__tests__/` — Output renderers
- `src/components/widgets/__tests__/` — Widget store, registry
- `src/lib/__tests__/` — ErrorBoundary
- `apps/notebook/src/hooks/__tests__/` — useEnvProgress
- `apps/notebook/src/lib/__tests__/` — Cursor registry, manifest resolution, materialize cells, kernel status, markdown assets

## Rust Unit Tests

```bash
cargo test                    # All workspace tests
cargo test -p runtimed        # Specific crate
cargo test -p notebook-doc    # Automerge doc tests
cargo test -- --nocapture     # Show println! output
```

**Key test crates:**

| Crate | Tests |
|-------|-------|
| `kernel-launch` | Tool hashing, path resolution |
| `notebook-doc` | Automerge document operations |
| `runtimed` | Blob store/server, connections, daemon, kernel manager, notebook sync, output store, protocol, runtime, settings, stream terminal |

## Hone CLI Tests

Declarative bash-based tests in `crates/runt/tests/*.hone`. Not yet published to crates.io.

```bash
cargo hone test               # All hone tests
cargo hone test cli.hone      # Specific file
```

**Available assertions:** `ASSERT exit_code == 0`, `ASSERT stdout contains "text"`, `ASSERT stdout matches /pattern/`, `ASSERT exit_code != 0`.

**Test files:** `cli.hone`, `kernel_lifecycle.hone`, `ps.hone`, `start_errors.hone`, `exec_errors.hone`, `interrupt_errors.hone`, `stop_errors.hone`.

## Python Tests (pytest)

### Two Venvs

| Venv | Path | Purpose |
|------|------|---------|
| Workspace venv | `.venv` (repo root) | MCP server and day-to-day dev |
| Test venv | `python/runtimed/.venv` | Isolated pytest runs |

### Setup

```bash
cd python/runtimed
python -m venv .venv
source .venv/bin/activate
pip install -e ".[dev]"

# Build native extension into test venv
cd ../../crates/runtimed-py
VIRTUAL_ENV=../../python/runtimed/.venv maturin develop
```

### Test Categories

| File | Type | Requires Daemon |
|------|------|-----------------|
| `test_session_unit.py` | Unit | No |
| `test_daemon_integration.py` | Integration | Yes |
| `test_ipython_bridge.py` | Integration | Yes |
| `test_binary.py` | Binary/CLI | No |

### Running

```bash
# Unit tests only (fast, no daemon)
pytest python/runtimed/tests/test_session_unit.py -v

# Skip integration tests
SKIP_INTEGRATION_TESTS=1 pytest python/runtimed/tests/ -v

# Integration tests (requires running dev daemon)
pytest python/runtimed/tests/test_daemon_integration.py -v

# CI mode (spawns its own daemon)
RUNTIMED_INTEGRATION_TEST=1 pytest python/runtimed/tests/ -v
```

### Environment Variables

| Variable | Effect |
|----------|--------|
| `SKIP_INTEGRATION_TESTS=1` | Skip tests marked `@pytest.mark.integration` |
| `RUNTIMED_INTEGRATION_TEST=1` | CI mode: spawns daemon automatically |
| `RUNTIMED_SOCKET_PATH` | Override daemon socket location |

## E2E Tests

### Running (Native Mode)

```bash
cargo xtask e2e build       # Build with WebDriver support
cargo xtask e2e test        # Smoke/default E2E run
cargo xtask e2e test-all    # Full suite, including fixture coverage
```

**Important:** Use `cargo xtask e2e build` (not plain `cargo build`) — the E2E binary embeds frontend assets and enables the webdriver feature.

### Fixture Tests

Fixture tests open a specific notebook and get a fresh app instance per test:

```bash
cargo xtask e2e test-fixture \
  crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
  e2e/specs/prewarmed-uv.spec.js
```

### Current Fixture Mapping

| Notebook | Spec | What it tests |
|----------|------|---------------|
| `1-vanilla.ipynb` | `prewarmed-uv.spec.js` | UV prewarmed environment pool |
| `2-uv-inline.ipynb` | `uv-inline.spec.js` | UV inline dependency resolution |
| `2-uv-inline.ipynb` | `trust-dialog-dismiss.spec.js` | Trust dialog dismiss flow |
| `3-conda-inline.ipynb` | `conda-inline.spec.js` | Conda inline dependency resolution |
| `10-deno.ipynb` | `deno.spec.js` | Deno kernel start + TypeScript execution |
| `pyproject-project/5-pyproject.ipynb` | `uv-pyproject.spec.js` | pyproject.toml environment detection |
| `14-cell-visibility.ipynb` | `cell-visibility.spec.js` | Cell source/output visibility toggling |
| `15-run-all-output-lifecycle.ipynb` | `run-all-output-lifecycle.spec.js` | Run-all output lifecycle |
| (directory-based pyproject fixture) | `untitled-pyproject.spec.js` | Untitled notebook with pyproject directory context |

### Adding a New Fixture Test

1. Choose or create a fixture notebook in `crates/notebook/fixtures/audit-test/`
2. Create the spec at `e2e/specs/my-feature.spec.js`
3. Add to `FIXTURE_SPECS` in `e2e/wdio.conf.js`
4. Add it to the fixture coverage in `crates/xtask/src/main.rs` if it should participate in `cargo xtask e2e test-all`
5. Add to CI in `.github/workflows/build.yml`
6. Verify locally with `cargo xtask e2e test-fixture ...`

### Adding a New Regular Test

1. Create the spec at `e2e/specs/my-feature.spec.js`
2. `cargo xtask e2e test` picks up non-fixture `*.spec.js` files automatically (anything not in `FIXTURE_SPECS`)

### Shared Helpers (e2e/helpers.js)

| Helper | What it does |
|--------|-------------|
| `waitForAppReady()` | Waits for toolbar (15s). Use in every `before()` hook. |
| `waitForKernelReady()` | Waits for kernel idle/busy (60s). Superset of `waitForAppReady()`. |
| `executeFirstCell()` | Focuses first code cell, hits Shift+Enter. Returns cell element. |
| `waitForCellOutput(cell)` | Waits for stream output. Returns text. |
| `waitForOutputContaining(cell, text)` | Waits for output containing specific text. |
| `waitForErrorOutput(cell)` | Waits for error output. Returns text. |
| `approveTrustDialog()` | Clicks "Trust & Install". |
| `typeSlowly(text)` | Character-by-character typing (30ms). Required for CodeMirror. |
| `setupCodeCell()` | Finds/creates code cell, focuses editor, selects all. |
| `waitForNotebookSynced()` | Waits for Automerge sync + cells rendered. |

### wry WebDriver Quirks

- **Text selectors don't work.** `$("button*=Code")` returns broken refs. Use `data-testid` attributes or `browser.execute()` with DOM APIs.
- **`browser.switchToFrame()` doesn't work.** Use the postMessage eval channel for iframe testing.
- **`browser.executeAsync()` not supported.** Use `browser.execute()` + `browser.waitUntil()` polling.
- **`browser.execute()` is your best friend.** Drop to raw DOM APIs when standard element methods fail.

### Design Patterns

- **Daemon-independent testing:** Never assert initial state. Always click first, then assert the result.
- **Iframe testing:** Use the `{ type: "eval" }` postMessage channel (production code's `frame-html.ts`).
- **Waiting:** Use `waitForAppReady()` for UI tests, `waitForKernelReady()` for code execution tests.
- **CodeMirror input:** Always use `typeSlowly()`. CodeMirror drops characters with fast input.

### Selectors Reference

`data-testid`: `notebook-toolbar`, `save-button`, `add-code-cell-button`, `add-markdown-cell-button`, `start-kernel-button`, `restart-kernel-button`, `interrupt-kernel-button`, `run-all-button`, `deps-toggle`, `trust-dialog`, `trust-approve-button`, `deps-panel`, `deps-add-input`.

`data-slot`: `output-area`, `ansi-stream-output`, `ansi-error-output`.

Other: `[data-cell-type="code"]`, `[data-cell-type="markdown"]`, `.cm-content[contenteditable="true"]`, `iframe[sandbox]`.

### Troubleshooting

- **"E2E binary not found"** — Run `cargo xtask e2e build`.
- **"No WebDriver server on port 4445"** — Run `cargo xtask e2e test` / `test-fixture` so xtask launches the app and waits for the embedded webdriver server.
- **"Malformed type for elementId"** — wry text-selector bug. Use `data-testid` selectors.
- **Timeout errors** — Kernel startup is slow on first run. Use 60s+ timeouts.
- **Flaky tests** — Use `waitUntil()` not `pause()`, use `typeSlowly()`, use `data-testid`.

## Test Philosophy

Prefer fast integration tests over slow E2E. Use E2E for critical user journeys, integration tests for daemon behavior, unit tests for algorithms.
