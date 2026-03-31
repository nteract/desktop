# Testing Guide

This guide covers all test types in the codebase. For E2E tests specifically, see [e2e.md](e2e.md).

## Quick Reference

| Type | Location | Command | Framework |
|------|----------|---------|-----------|
| E2E | `e2e/specs/` | `cargo xtask e2e test` | WebdriverIO + Mocha |
| Frontend unit | `src/**/__tests__/`, `apps/notebook/src/**/__tests__/` | `pnpm test` | Vitest + jsdom |
| Rust unit | inline `#[cfg(test)]` | `cargo test` | built-in |
| CLI behavior | `crates/runt/tests/*.hone` | `cargo hone test` | Hone (not yet published) |
| Python | `python/runtimed/tests/` | `pytest` | pytest |

## Frontend Unit Tests (Vitest)

Configuration: `vitest.config.ts`

```ts
test: {
  environment: "jsdom",
  include: [
    "src/**/__tests__/**/*.test.{ts,tsx}",
    "apps/notebook/src/**/__tests__/**/*.test.{ts,tsx}",
    "packages/**/tests/**/*.test.{ts,tsx}",
  ],
  globals: true,
  setupFiles: ["./src/test-setup.ts"],
}
```

**Running tests:**

```bash
pnpm test         # Watch mode
pnpm test:run     # Run once
```

**Writing tests:**

```tsx
// src/components/outputs/__tests__/ansi-output.test.tsx
import { render } from "@testing-library/react";
import { AnsiOutput } from "../ansi-output";

describe("AnsiOutput", () => {
  it("renders plain text", () => {
    const { container } = render(<AnsiOutput>{"hello"}</AnsiOutput>);
    expect(container.textContent).toBe("hello");
  });

  it.each([
    ["red", "\x1b[31mred\x1b[0m"],
    ["green", "\x1b[32mgreen\x1b[0m"],
  ])("renders %s ANSI color", (_, text) => {
    const { container } = render(<AnsiOutput>{text}</AnsiOutput>);
    expect(container.querySelector(".ansi-red-fg")).toBeTruthy();
  });
});
```

**Test locations:**

- `src/components/isolated/__tests__/` — Frame bridge, message protocol
- `src/components/outputs/__tests__/` — Output renderers
- `src/components/widgets/__tests__/` — Widget store, registry
- `src/lib/__tests__/` — ErrorBoundary
- `apps/notebook/src/hooks/__tests__/` — useEnvProgress
- `apps/notebook/src/lib/__tests__/` — Cursor registry, manifest resolution, materialize cells, kernel status, markdown assets, and more

## Rust Unit Tests

Rust tests are inline modules using `#[cfg(test)]`:

```rust
// crates/runtimed/src/runtime.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_serde() {
        let runtime = Runtime::Python;
        let json = serde_json::to_string(&runtime).unwrap();
        assert_eq!(json, "\"python\"");
    }
}
```

**Running tests:**

```bash
cargo test                    # All workspace tests
cargo test -p runtimed        # Specific crate
cargo test -p notebook-doc    # Automerge doc tests
cargo test -- --nocapture     # Show println! output
```

**Key test locations:**

| Crate | Tests |
|-------|-------|
| `kernel-launch` | Tool hashing, path resolution |
| `notebook-doc` | Automerge document operations |
| `runtimed` | Blob store/server, connections, daemon, kernel manager, notebook sync, output store, protocol, runtime, settings, stream terminal, and more |

## Hone CLI Tests

Hone is a bash-based declarative test framework for the `runt` CLI. Test files are in `crates/runt/tests/*.hone`.

**File format:**

```bash
#! shell: /bin/bash
#! timeout: 60s

TEST "help flag displays usage"
RUN runt --help
ASSERT exit_code == 0
ASSERT stdout contains "Usage: runt [COMMAND]"

TEST "invalid command fails"
RUN runt invalid_command
ASSERT exit_code != 0
ASSERT stderr contains "error: unrecognized subcommand"

TEST "version matches regex"
RUN runt --version
ASSERT stdout matches /runt-cli \d+\.\d+\.\d+/
```

**Available assertions:**

| Assertion | Example |
|-----------|---------|
| Exit code | `ASSERT exit_code == 0` |
| Contains | `ASSERT stdout contains "text"` |
| Regex match | `ASSERT stdout matches /pattern/` |
| Not equal | `ASSERT exit_code != 0` |

**Test files:**

| File | Coverage |
|------|----------|
| `cli.hone` | Help, version, invalid commands |
| `kernel_lifecycle.hone` | Start, execute, stop, interrupt |
| `ps.hone` | Process listing |
| `start_errors.hone` | Invalid kernel errors |
| `exec_errors.hone` | Execution error handling |
| `interrupt_errors.hone` | Interrupt signal handling |
| `stop_errors.hone` | Stop command edge cases |

**Running Hone tests:**

> **Note:** `cargo hone` is not yet published to crates.io. The `.hone` test files exist in the repo but the test runner is not currently installable. This section will be updated once `hone` is available.

```bash
cargo hone test               # All hone tests
cargo hone test cli.hone      # Specific file
```

## Python Tests (pytest)

Location: `python/runtimed/tests/`

**Virtual environments:** There are two Python venvs in this repo:

| Venv | Path (from repo root) | Purpose |
|------|-----------------------|---------|
| Workspace venv | `.venv` | Used by the MCP server and day-to-day development. `maturin develop` installs here. |
| Test venv | `python/runtimed/.venv` | Isolated env for `pytest` runs against `runtimed-py`. |

Set up the test venv:

```bash
cd python/runtimed
python -m venv .venv
source .venv/bin/activate
pip install -e ".[dev]"

# Build the native extension into the test venv
cd ../../crates/runtimed-py
VIRTUAL_ENV=../../python/runtimed/.venv maturin develop
```

> **Tip:** The workspace venv at `.venv` (repo root) is a separate concern — the MCP server and other workspace tooling use it. To install the bindings there instead, run `VIRTUAL_ENV=../../.venv maturin develop` from `crates/runtimed-py`.

Source-built daemon and MCP flows default to the nightly channel. Set `RUNT_BUILD_CHANNEL=stable` only when a test is intentionally validating stable-specific naming or paths.

Configuration in `conftest.py` defines markers and daemon detection.

**Test categories:**

| File | Type | Requires Daemon |
|------|------|-----------------|
| `test_session_unit.py` | Unit | No |
| `test_daemon_integration.py` | Integration | Yes |
| `test_ipython_bridge.py` | Unit-style bridge test | No |
| `test_binary.py` | Binary/CLI | No |

**Running tests:**

```bash
# Unit tests only (fast, no daemon)
pytest python/runtimed/tests/test_session_unit.py -v

# Skip integration tests
SKIP_INTEGRATION_TESTS=1 pytest python/runtimed/tests/ -v

# Integration tests (requires running dev daemon and an explicit socket)
RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])'
)" pytest python/runtimed/tests/test_daemon_integration.py -v

# CI mode (spawns its own daemon)
RUNTIMED_INTEGRATION_TEST=1 pytest python/runtimed/tests/ -v
```

When Python code should honor that exported socket, use `default_socket_path()`. Use `socket_path_for_channel("stable"|"nightly")` only for tests that intentionally target a specific release channel.

**Writing tests:**

```python
# Unit test (no daemon)
class TestModuleExports:
    def test_client_exported(self):
        assert hasattr(runtimed, "Client")

    def test_notebook_exported(self):
        assert hasattr(runtimed, "Notebook")

# Integration test (needs daemon, uses NativeAsyncClient for direct session access)
@pytest.mark.asyncio
async def test_kernel_execution(async_session):
    await async_session.start_kernel()
    result = await async_session.run("1 + 1")
    assert result.success
```

**Environment variables:**

| Variable | Effect |
|----------|--------|
| `SKIP_INTEGRATION_TESTS=1` | Skip tests marked `@pytest.mark.integration` |
| `RUNTIMED_INTEGRATION_TEST=1` | CI mode: spawns daemon automatically |
| `RUNTIMED_SOCKET_PATH` | Override daemon socket location |

## E2E Tests

See [e2e.md](e2e.md) for the full guide.

Quick start:

```bash
cargo xtask e2e build       # Build the webdriver-enabled app
cargo xtask e2e test        # Smoke/default E2E run
cargo xtask e2e test-all    # All regular + fixture specs
```

## Test Philosophy

From [architecture.md](architecture.md):

- **E2E tests** (WebdriverIO): Slow but comprehensive, test full user journeys
- **Integration tests** (Python bindings): Fast daemon interaction tests
- **Unit tests**: Pure logic, no I/O, fast feedback

Preference: Fast integration tests over slow E2E where possible. Use E2E for critical user journeys, integration tests for daemon behavior, unit tests for algorithms.
