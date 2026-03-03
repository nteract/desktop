# E2E Testing

End-to-end tests for the notebook application using WebdriverIO and Tauri's WebDriver.

## Why Docker?

Tauri's WebDriver on macOS is blocked by sandboxing restrictions. We run tests in a Linux Docker container with Xvfb for headless display rendering.

## Quick Start

### CI Mode (Full Build)

For CI pipelines or first-time setup. Builds everything from scratch:

```bash
pnpm test:e2e:docker
```

This takes 4-5 minutes due to Rust compilation.

### Native Mode (macOS, faster iteration)

For local development on macOS with the built-in WebDriver server:

```bash
# Build with WebDriver support
./e2e/dev.sh build

# Start app with WebDriver server (in one terminal)
./e2e/dev.sh start

# Run tests (in another terminal)
./e2e/dev.sh test
```

### Interactive Debugging

Drop into a shell with the Docker test environment ready:

```bash
pnpm e2e:docker:shell
```

Inside the container:
```bash
# Run all tests
pnpm test:e2e

# Run a single spec file
pnpm wdio run e2e/wdio.conf.js --spec e2e/specs/smoke.spec.js
```

## Test Files

| File | Description |
|------|-------------|
| `specs/smoke.spec.js` | Basic cell execution and output |
| `specs/prewarmed-uv.spec.js` | Prewarmed UV environment pool |
| `specs/uv-inline.spec.js` | UV inline dependency resolution |
| `specs/conda-inline.spec.js` | Conda inline dependency resolution |
| `specs/deno.spec.js` | Deno kernel start + TypeScript execution |
| `specs/uv-pyproject.spec.js` | pyproject.toml environment detection |
| `specs/tab-completion.spec.js` | Tab completion in code cells |
| `specs/untitled-pyproject.spec.js` | Untitled notebook in pyproject.toml directory |

## npm Scripts

| Script | Description |
|--------|-------------|
| `test:e2e` | Run tests with WebdriverIO |
| `test:e2e:docker` | Run tests in Docker container |
| `test:e2e:native` | Run tests natively (same as test:e2e) |
| `e2e:docker:build` | Build the Docker image |
| `e2e:docker:shell` | Interactive shell for debugging |

## Writing Tests

Tests use WebdriverIO with Mocha. Key patterns:

```javascript
import { browser, expect } from "@wdio/globals";

describe("My Feature", () => {
  it("should do something", async () => {
    // Find elements using CSS selectors
    const cell = await $('[data-cell-type="code"]');

    // Interact with elements
    await cell.click();
    await browser.keys("print('hello')");

    // Wait for async operations
    await browser.waitUntil(async () => {
      const output = await $('[data-slot="ansi-stream-output"]');
      return output.isExisting();
    }, { timeout: 30000 });

    // Assert
    const text = await output.getText();
    expect(text).toContain("hello");
  });
});
```

## Selectors

Use these data attributes for reliable element selection:

| Selector | Element |
|----------|---------|
| `[data-cell-type="code"]` | Code cell container |
| `[data-cell-id="..."]` | Cell by ID |
| `[data-testid="execute-button"]` | Run cell button |
| `[data-slot="ansi-stream-output"]` | Stream output (stdout/stderr) |
| `.cm-content[contenteditable="true"]` | CodeMirror editor |

## Timeouts

- **App load**: 5 seconds
- **Kernel startup**: 30-60 seconds (first execution)
- **Cell execution**: 15 seconds (after kernel is ready)
- **Element appear**: 5 seconds
