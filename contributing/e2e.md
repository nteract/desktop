# E2E Testing Guide

End-to-end tests verify the full application from a user's perspective. We use **WebdriverIO** + **Mocha** over the **W3C WebDriver protocol** to drive the Tauri app.

Two modes:

1. **Native mode (macOS)** — Built-in WebDriver server. No Docker needed.
2. **Docker mode (Linux/CI)** — `tauri-driver` + `webkit2gtk-driver` in a container.

## Running Tests

### Native Mode (macOS)

The `e2e/dev.sh` script handles everything:

```bash
# Full cycle: build + start + test
./e2e/dev.sh cycle

# Step by step:
./e2e/dev.sh build       # Build with WebDriver support (embeds frontend)
./e2e/dev.sh start       # Start app with WebDriver server (foreground)
./e2e/dev.sh test        # Smoke test (notebook-execution only)
./e2e/dev.sh test all    # All non-fixture specs
./e2e/dev.sh stop        # Stop the running app
```

**Important:** Use `./e2e/dev.sh build` (or `cargo xtask build-e2e`) instead of plain `cargo build`. The Tauri build embeds frontend assets into the binary — a plain `cargo build` would try to connect to a Vite dev server.

#### Fixture Tests

Fixture tests open a specific notebook and get a fresh app instance per test:

```bash
# Run a single fixture test
./e2e/dev.sh test-fixture \
  crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
  e2e/specs/vanilla-startup.spec.js

# Run all fixture tests (fresh app per test, exits 1 if any fail)
./e2e/dev.sh test-fixtures
```

#### All dev.sh Commands

| Command | Description |
|---------|-------------|
| `build` | Rebuild Rust binary (incremental, embeds frontend) |
| `build-full` | Full rebuild (frontend + sidecars + Rust) |
| `start` | Start app with WebDriver server (foreground) |
| `stop` | Stop the running app |
| `restart` | Stop + start |
| `test [spec\|all]` | Run E2E tests (default: notebook-execution only) |
| `test-fixture <nb> <spec>` | Run a fixture test (fresh app per test) |
| `test-fixtures` | Run all fixture tests |
| `cycle` | Build + start + test in one shot |
| `status` | Check if WebDriver server is running |
| `session` | Create a session and print ID |
| `exec 'js'` | Execute JS in the app |

### Port Configuration

The WebDriver port uses this fallback chain (same in `dev.sh` and `wdio.conf.js`):

```
WEBDRIVER_PORT > CONDUCTOR_PORT > PORT > 4444
```

Most contributors don't need to set anything — the default `4444` works fine.

### Docker Mode (CI / Linux)

```bash
pnpm test:e2e:docker

# Interactive debugging
docker compose --profile dev run --rm tauri-e2e-shell
```

## Test Types

### Regular Tests vs. Fixture Tests

There are two kinds of E2E specs:

**Regular specs** run against whatever notebook the app opens by default. They share a single app instance during `./e2e/dev.sh test all`. Good for testing general UI features that don't depend on specific notebook content.

**Fixture specs** require a specific notebook (`NOTEBOOK_PATH` env var) and get a fresh app instance per test. Each is listed in `FIXTURE_SPECS` in `wdio.conf.js` so it's automatically excluded from the default `test all` run.

Use a fixture test when:
- The test needs specific pre-populated cell content (e.g., `import sys; print(sys.executable)`)
- The test needs a clean app state (no leftover cells/outputs from prior tests)
- The test exercises features tied to notebook content (deps panel, trust dialog, environment detection)

Use a regular test when:
- The test creates its own cells and doesn't depend on pre-existing content
- The test is about general UI behavior (cell operations, keyboard shortcuts, markdown editing)

### Current Fixture Mapping

| Notebook | Spec | What it tests |
|----------|------|---------------|
| `1-vanilla.ipynb` | `vanilla-startup.spec.js` | UV prewarmed env startup |
| `1-vanilla.ipynb` | `settings-panel.spec.js` | Settings panel UI (daemon-independent) |
| `2-uv-inline.ipynb` | `uv-inline-deps.spec.js` | UV inline dependency resolution |
| `2-uv-inline.ipynb` | `deps-panel.spec.js` | UV deps panel UI |
| `2-uv-inline.ipynb` | `trust-decline.spec.js` | Trust dialog rejection |
| `3-conda-inline.ipynb` | `conda-inline-deps.spec.js` | Conda inline dependency resolution |
| `3-conda-inline.ipynb` | `conda-deps-panel.spec.js` | Conda deps panel UI |
| `4-both-deps.ipynb` | `both-deps-panel.spec.js` | Dual UV + Conda deps |
| `5-pyproject.ipynb` | `pyproject-startup.spec.js` | pyproject.toml environment detection |
| `6-pixi.ipynb` | `pixi-env-detection.spec.js` | pixi.toml environment detection |
| `7-environment-yml.ipynb` | `environment-yml-detection.spec.js` | environment.yml detection |
| `8-multi-cell.ipynb` | `run-all-cells.spec.js` | Run All / Restart & Run All |
| `9-html-output.ipynb` | `iframe-isolation.spec.js` | Iframe sandbox security |
| `10-deno.ipynb` | `deno-runtime.spec.js` | Deno kernel start + TypeScript execution |
| `1-vanilla.ipynb` | `save-dirty-state.spec.js` | Save button dirty state indicator |
| `11-rich-outputs.ipynb` | `rich-outputs.spec.js` | Rich output rendering (PNG, HTML, DataFrame, ANSI) |
| `12-error-outputs.ipynb` | `error-handling.spec.js` | Error traceback rendering |

Multiple specs can reuse the same fixture notebook — each gets its own fresh app instance.

## Adding a New Test

### Checklist: New Fixture Test

1. **Choose or create a fixture notebook** in `crates/notebook/fixtures/audit-test/`. Reuse an existing one if possible.

2. **Create the spec** at `e2e/specs/my-feature.spec.js`:
   ```javascript
   /**
    * E2E Test: My Feature (Fixture)
    *
    * Description of what this tests.
    *
    * Requires: NOTEBOOK_PATH=crates/notebook/fixtures/audit-test/1-vanilla.ipynb
    */
   import { browser, expect } from "@wdio/globals";
   import { waitForAppReady } from "../helpers.js";

   describe("My Feature", () => {
     before(async () => {
       await waitForAppReady();
     });

     it("should do the thing", async () => {
       // ...
     });
   });
   ```

3. **Add to `FIXTURE_SPECS`** in `e2e/wdio.conf.js`:
   ```javascript
   const FIXTURE_SPECS = [
     // ... existing entries
     "my-feature.spec.js",
   ];
   ```

4. **Add to `test-fixtures`** in `e2e/dev.sh`:
   ```bash
   $0 test-fixture \
     crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
     e2e/specs/my-feature.spec.js || FAIL=1
   ```

5. **Add to CI** in `.github/workflows/build.yml`:
   ```yaml
   start_driver
   NOTEBOOK_PATH=crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
     E2E_SPEC=e2e/specs/my-feature.spec.js \
     pnpm test:e2e || FAIL=1
   ```

6. **Verify locally:**
   ```bash
   ./e2e/dev.sh build
   ./e2e/dev.sh test-fixture \
     crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
     e2e/specs/my-feature.spec.js
   ```

### Checklist: New Regular Test

1. Create the spec at `e2e/specs/my-feature.spec.js` (same structure, no `Requires:` comment).
2. That's it — `test all` picks up `*.spec.js` files automatically (anything not in `FIXTURE_SPECS`).

## Shared Helpers

Import from `e2e/helpers.js`:

```javascript
import {
  waitForAppReady,
  waitForKernelReady,
  executeFirstCell,
  // ... etc
} from "../helpers.js";
```

| Helper | What it does |
|--------|-------------|
| `waitForAppReady()` | Waits for the toolbar to appear (15s). Use in every `before()` hook. |
| `waitForKernelReady()` | Waits for kernel to reach `idle` or `busy` (30s). Superset of `waitForAppReady()`. |
| `executeFirstCell()` | Focuses the first code cell's editor and hits Shift+Enter. Returns the cell element. |
| `waitForCellOutput(cell, timeout?)` | Waits for stream output to appear in a cell. Returns the text. |
| `waitForOutputContaining(cell, text, timeout?)` | Waits for stream output containing specific text. Returns the full text. |
| `waitForErrorOutput(cell, timeout?)` | Waits for error output to appear. Returns the text. |
| `approveTrustDialog(timeout?)` | Waits for the trust dialog and clicks "Trust & Install". |
| `getKernelStatus()` | Returns the current kernel status string (e.g., `"idle"`, `"busy"`, `"starting"`). |
| `waitForKernelStatus(status, timeout?)` | Waits for the kernel to reach a specific status. |
| `typeSlowly(text, delay?)` | Types character-by-character (30ms default). Use for CodeMirror input. |
| `findButton(patterns[])` | Tries CSS selectors in order, returns first match or null. |
| `setupCodeCell()` | Finds or creates a code cell, focuses editor, selects all. Returns the cell. |

Platform note: `MOD_KEY` is `"Meta"` on macOS, `"Control"` on Linux. Used internally by `executeFirstCell()` and `setupCodeCell()`.

## wry WebDriver Quirks

The Tauri WebView engine (wry) has a custom WebDriver implementation with some important limitations. These will save you debugging time.

### Text selectors don't work

WebdriverIO text-content selectors like `$("button*=Code")` return element references with `undefined` elementId in wry. Any subsequent interaction with those elements fails.

```javascript
// BAD — returns broken element reference in wry
const button = await $("button*=Code");
await button.click(); // "Malformed type for elementId parameter"

// GOOD — use data-testid
const button = await $('[data-testid="add-code-cell-button"]');
await button.click();
```

Always add `data-testid` attributes and use them for element selection. If you need to find a button by its visible text, use `browser.execute()`:

```javascript
// Find button by text content via browser.execute()
const clicked = await browser.execute(() => {
  const buttons = document.querySelectorAll("button");
  for (const btn of buttons) {
    if (btn.textContent?.includes("Dark")) {
      btn.click();
      return true;
    }
  }
  return false;
});
```

### `browser.switchToFrame()` doesn't work

wry's WebDriver does not support switching iframe context. `browser.execute()` always runs in the parent frame regardless of `switchToFrame()` calls.

To test iframe internals, use the **postMessage eval channel** that the production code already provides (see [Iframe Testing](#iframe-testing) below).

### `browser.executeAsync()` is not supported

wry returns 404 for the `/session/.../execute_async` endpoint. Use `browser.execute()` + `browser.waitUntil()` polling instead:

```javascript
// BAD — 404 in wry
const result = await browser.executeAsync((done) => {
  setTimeout(() => done("value"), 100);
});

// GOOD — polling pattern
await browser.execute(() => {
  window.__myResult = undefined;
  setTimeout(() => { window.__myResult = "value"; }, 100);
});
await browser.waitUntil(
  async () => await browser.execute(() => window.__myResult !== undefined),
  { timeout: 5000 }
);
const result = await browser.execute(() => window.__myResult);
```

### `browser.execute()` is your best friend

When WebdriverIO's standard element methods don't work reliably in wry, drop down to `browser.execute()` with raw DOM APIs. This goes through the JS bridge directly and is always reliable:

```javascript
// Check if an element exists
const exists = await browser.execute(() => {
  return !!document.querySelector('[data-testid="my-element"]');
});

// Click a button
await browser.execute(() => {
  document.querySelector('[data-testid="my-button"]').click();
});

// Read a class list
const isDark = await browser.execute(() => {
  return document.documentElement.classList.contains("dark");
});
```

## Design Patterns

### Daemon-Independent Testing

Some features interact with the global runtimed daemon (Automerge-based settings sync). If the daemon is running, it may override default values on mount. If it's not running, the app falls back to localStorage.

**The rule: never assert initial state. Always click first, then assert the result.**

```javascript
// BAD — fragile: daemon may have set theme to "dark" already
it("should start with system theme", async () => {
  const theme = await getThemeSetting();
  expect(theme).toBe("system"); // fails if daemon set it to "dark"
});

// GOOD — tests the observable effect of an action
it("should apply dark class when clicking Dark", async () => {
  // Click Dark (regardless of what the initial state was)
  await browser.execute(() => {
    const btn = document.querySelector('[data-testid="settings-theme-group"] button');
    // find the Dark button and click it
  });

  // Assert the observable DOM effect
  await browser.waitUntil(async () => {
    return await browser.execute(() =>
      document.documentElement.classList.contains("dark")
    );
  });
});
```

This pattern applies to any test touching settings, preferences, or state that could be influenced by external processes. The test should work identically whether or not the daemon is running.

### Iframe Testing

wry doesn't support `browser.switchToFrame()`, but the production code's `frame-html.ts` handles `{ type: "eval" }` postMessage messages. We can use this existing channel to run code inside the iframe:

```javascript
async function evalInIframe(code, timeout = 10000) {
  // Set up listener in parent, send eval to iframe
  await browser.execute((code) => {
    window.__iframeEvalResult = undefined;
    window.__iframeEvalDone = false;

    window.addEventListener("message", function handler(event) {
      if (event.data?.type === "eval_result") {
        window.__iframeEvalResult = event.data.payload;
        window.__iframeEvalDone = true;
        window.removeEventListener("message", handler);
      }
    });

    const iframe = document.querySelector('iframe[title="Isolated output frame"]');
    iframe?.contentWindow?.postMessage(
      { type: "eval", payload: { code } }, "*"
    );
  }, code);

  // Poll until result arrives
  await browser.waitUntil(
    async () => await browser.execute(() => window.__iframeEvalDone === true),
    { timeout, interval: 100 }
  );

  return await browser.execute(() => window.__iframeEvalResult);
}

// Usage
const result = await evalInIframe("typeof window.__TAURI_INTERNALS__");
expect(result.success).toBe(true);
expect(result.result).toBe("undefined"); // Tauri API not leaked
```

### Waiting for Kernel vs. App

Choose the right wait based on what your test needs:

- **`waitForAppReady()`** — Use when testing UI-only features (settings panel, cell management, markdown editing). Faster, doesn't require a kernel.
- **`waitForKernelReady()`** — Use when the test will execute code. Includes `waitForAppReady()` internally, then waits for the kernel to reach `idle` or `busy`.

### Typing into CodeMirror

Always use `typeSlowly()` from helpers. CodeMirror drops characters with fast bulk input:

```javascript
import { typeSlowly, setupCodeCell } from "../helpers.js";

const cell = await setupCodeCell(); // finds/creates cell, focuses editor, selects all
await typeSlowly('print("hello")');
await browser.keys(["Shift", "Enter"]); // execute
```

## Selectors Reference

### `data-testid` Attributes

| Selector | Element | Component |
|----------|---------|-----------|
| `notebook-toolbar` | Main toolbar container | `NotebookToolbar` |
| `save-button` | Save notebook | `NotebookToolbar` |
| `add-code-cell-button` | Add code cell | `NotebookToolbar` |
| `add-markdown-cell-button` | Add markdown cell | `NotebookToolbar` |
| `start-kernel-button` | Start kernel | `NotebookToolbar` |
| `restart-kernel-button` | Restart kernel | `NotebookToolbar` |
| `interrupt-kernel-button` | Interrupt kernel | `NotebookToolbar` |
| `run-all-button` | Run all cells | `NotebookToolbar` |
| `restart-run-all-button` | Restart & run all | `NotebookToolbar` |
| `deps-toggle` | Toggle deps panel | `NotebookToolbar` |
| `settings-panel` | Settings panel container | `NotebookToolbar` |
| `settings-theme-group` | Theme button group | `NotebookToolbar` |
| `settings-runtime-group` | Runtime button group | `NotebookToolbar` |
| `settings-python-env-group` | Python env button group | `NotebookToolbar` |
| `execute-button` | Run cell button | `CodeCell` |
| `trust-dialog` | Trust dialog overlay | `TrustDialog` |
| `trust-approve-button` | "Trust & Install" button | `TrustDialog` |
| `trust-decline-button` | "Don't Trust" button | `TrustDialog` |
| `deps-panel` | UV deps panel | `DepsPanel` |
| `deps-add-input` | UV dep input field | `DepsPanel` |
| `deps-add-button` | UV dep add button | `DepsPanel` |
| `conda-deps-panel` | Conda deps panel | `CondaDepsPanel` |
| `conda-deps-add-input` | Conda dep input field | `CondaDepsPanel` |
| `conda-deps-add-button` | Conda dep add button | `CondaDepsPanel` |

### `data-slot` Attributes

| Selector | Element |
|----------|---------|
| `[data-slot="output-area"]` | Cell output area |
| `[data-slot="ansi-stream-output"]` | Stream output (stdout/stderr) |
| `[data-slot="ansi-error-output"]` | Error output with traceback |

### Other Selectors

| Selector | Element |
|----------|---------|
| `[data-cell-type="code"]` | Code cell container |
| `[data-cell-type="markdown"]` | Markdown cell container |
| `[data-cell-id="..."]` | Cell by specific ID |
| `.cm-content[contenteditable="true"]` | CodeMirror editor |
| `iframe[sandbox]` | Isolated output iframe |
| `iframe[title="Isolated output frame"]` | Named isolated iframe |
| `[aria-label="Settings"]` | Settings gear button |

### Adding New `data-testid` Attributes

Always add `data-testid` for any interactive element that E2E tests might need. This is cheap to add and prevents flaky tests down the road.

```tsx
<button
  onClick={handleClick}
  data-testid="my-feature-button"
>
  Click me
</button>
```

Naming: kebab-case, specific (`cell-delete-button` not `delete`), match component names when sensible.

## Architecture

### Native Mode

```
┌──────────────┐    W3C WebDriver    ┌──────────────────────────┐
│  WebdriverIO │    HTTP protocol    │   notebook binary        │
│  Test Runner │ ◄─────────────────► │                          │
│              │    localhost:$PORT   │  ┌────────────────────┐  │
│  (test specs)│                     │  │ WebDriver Server   │  │
│              │                     │  │ (axum HTTP server)  │  │
│              │                     │  └────────┬───────────┘  │
└──────────────┘                     │           │              │
                                     │   eval()  │  fetch()     │
                                     │           ▼              │
                                     │  ┌────────────────────┐  │
                                     │  │ WebView            │  │
                                     │  │  ┌──────────────┐  │  │
                                     │  │  │ Test Bridge   │  │  │
                                     │  │  │ (injected JS) │  │  │
                                     │  │  └──────────────┘  │  │
                                     │  └────────────────────┘  │
                                     └──────────────────────────┘
```

The built-in WebDriver server:
1. Receives W3C WebDriver HTTP requests from WebdriverIO
2. Translates them to JavaScript and executes via `webview.eval()`
3. The JS bridge executes DOM operations and sends results back via `fetch()`
4. Results are returned as WebDriver HTTP responses

### Docker Mode

Same WebdriverIO tests, but the app runs inside a Docker container with
`tauri-driver` + `webkit2gtk-driver` providing the WebDriver protocol bridge.

## Test Configuration

Configuration is in `e2e/wdio.conf.js`:

- **maxInstances**: 1 (single Tauri app instance)
- **timeout**: 180000ms per test (3 minutes, for kernel startup scenarios)
- **waitforTimeout**: 10000ms for `waitFor*` methods
- **connectionRetryTimeout**: 120000ms for WebDriver connection
- **Screenshots**: On failure, saved to `e2e-screenshots/failures/` (configurable via `E2E_SCREENSHOT_DIR`)

### Environment Variables

| Variable | Purpose |
|----------|---------|
| `E2E_SPEC` | Run a single spec file (overrides default glob + exclude) |
| `NOTEBOOK_PATH` | Fixture notebook path (passed as CLI arg to the binary) |
| `WEBDRIVER_PORT` | WebDriver port (explicit override) |
| `CONDUCTOR_PORT` | Conductor workspace port (auto-set in parallel worktrees) |
| `TAURI_APP_PATH` | Path to compiled binary (default: `/app/target/release/notebook`) |
| `E2E_SCREENSHOT_DIR` | Screenshot output directory |

## Timeout Guidelines

| Operation | Timeout | Notes |
|-----------|---------|-------|
| App load (`waitForAppReady`) | 15s | Toolbar mounting |
| Kernel startup (`waitForKernelReady`) | 30s | First kernel start can be slow |
| Cell execution | 120s (default) | Environment creation on first run |
| Element appear | 5s | DOM rendering |
| Button clickable | 5s | React hydration |
| Synchronous DOM effects (theme, panel toggle) | 2-3s | React re-renders, no I/O |

## Debugging

```javascript
// Log progress
console.log("Step completed:", someValue);

// Inspect page state
console.log("Title:", await browser.getTitle());
const html = await browser.getPageSource();

// Run arbitrary JS in the app
const result = await browser.execute(() => {
  return document.querySelector('[data-cell-type]')?.outerHTML;
});

// Pause to observe behavior
await browser.pause(5000);
```

### dev.sh Shortcuts

```bash
# Quick JS evaluation against the running app
./e2e/dev.sh exec 'return document.title'

# Check if WebDriver is up
./e2e/dev.sh status
```

## Troubleshooting

### "E2E binary not found"

The WebDriver-enabled binary hasn't been built yet. `dev.sh` will tell you exactly what to run:

```bash
./e2e/dev.sh build       # fast: recompiles Rust only (skips frontend)
./e2e/dev.sh build-full  # full: rebuilds frontend + sidecars + Rust
```

Use `build-full` the first time or after changing React components (e.g., adding `data-testid`). After that, `build` is much faster for Rust-only changes.

### "No WebDriver server on port 4444"

You ran `./e2e/dev.sh test` without starting the app first. The `test` command connects to an already-running app — it does **not** start one.

Either:
- Start the app in one terminal (`./e2e/dev.sh start`) and run tests in another (`./e2e/dev.sh test`)
- Use `./e2e/dev.sh cycle` to build + start + test in one shot
- Use `./e2e/dev.sh test-fixture` which starts a fresh app instance automatically

**Key distinction:** `test` requires a running app. `test-fixture` manages its own app lifecycle.

### "Notebook file not found" / "Spec file not found"

Paths are relative to the project root. `dev.sh` will list available fixtures and specs when it can't find a file.

```bash
# Correct:
./e2e/dev.sh test-fixture crates/notebook/fixtures/audit-test/1-vanilla.ipynb e2e/specs/vanilla-startup.spec.js

# Wrong — don't use absolute paths or paths from other directories:
./e2e/dev.sh test-fixture /Users/me/runt/crates/notebook/fixtures/audit-test/1-vanilla.ipynb ...
```

### "Malformed type for elementId parameter"

You're hitting the wry text-selector bug. Replace `$("button*=Text")` with `$('[data-testid="..."]')`. See [wry WebDriver Quirks](#wry-webdriver-quirks).

### "No such element" Errors

- Element may not be rendered yet — add `waitForExist()` or `waitForClickable()`
- Selector may be wrong — verify with `browser.getPageSource()`
- Element may be in an iframe — use the postMessage eval pattern (not `switchToFrame`)

### Connection failures (11 WebDriver errors)

If you see many `Request failed with status 500` or connection errors, the app is not running or not listening on the expected port. Check:

```bash
./e2e/dev.sh status    # should print JSON response, not "Not running"
```

The port fallback chain is `WEBDRIVER_PORT > CONDUCTOR_PORT > PORT > 4444`. If you're in a worktree with `CONDUCTOR_PORT` set, tests will use that port — make sure the app was started with the same port.

### Timeout Errors

- Kernel startup is slow on first run — increase timeout to 60s
- Environment creation can take 2+ minutes — fixture tests default to 120s
- Check if the app loaded correctly with `./e2e/dev.sh status`

### Flaky Tests

- Use `waitUntil()` for async conditions, never `pause()` as a gate
- Use `typeSlowly()` for CodeMirror input
- Use `data-testid` selectors instead of text selectors
- If testing features with daemon interaction, follow the [daemon-independent pattern](#daemon-independent-testing)
- On CI (Linux), React re-renders may not be atomic — use `waitUntil()` to poll for class changes rather than asserting immediately after a click

### Docker Build Issues

```bash
# Force rebuild without cache
docker compose build --no-cache tauri-e2e

# Inspect container for debugging
docker compose --profile dev run --rm tauri-e2e-shell
```
