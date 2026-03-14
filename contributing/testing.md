# Testing Guide

This guide covers all test types in the codebase. For E2E tests specifically, see [e2e.md](e2e.md).

## Quick Reference

| Type | Location | Command | Framework |
|------|----------|---------|-----------|
| E2E | `e2e/specs/` | `./e2e/dev.sh test` | WebdriverIO + Mocha |
| Frontend unit | `src/**/__tests__/` | `pnpm test` | Vitest + jsdom |
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
| `runtimed` | Settings, kernel manager, stream terminal |

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

Configuration in `conftest.py` defines markers and daemon detection.

**Test categories:**

| File | Type | Requires Daemon |
|------|------|-----------------|
| `test_session_unit.py` | Unit | No |
| `test_daemon_integration.py` | Integration | Yes |
| `test_ipython_bridge.py` | Integration | Yes |
| `test_binary.py` | Binary/CLI | No |

**Running tests:**

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

**Writing tests:**

```python
# Unit test (no daemon)
class TestSessionConstruction:
    def test_session_with_auto_id(self):
        session = runtimed.Session()
        assert session.notebook_id.startswith("agent-session-")
        assert not session.is_connected

# Integration test (needs daemon)
@pytest.mark.asyncio
async def test_kernel_execution(self):
    session = runtimed.AsyncSession()
    await session.connect()
    result = await session.execute("1 + 1")
    assert "2" in result
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
./e2e/dev.sh cycle          # Build + start + test
./e2e/dev.sh test           # Smoke test only
./e2e/dev.sh test all       # All non-fixture specs
```

## Test Philosophy

From [architecture.md](architecture.md):

- **E2E tests** (WebdriverIO): Slow but comprehensive, test full user journeys
- **Integration tests** (Python bindings): Fast daemon interaction tests
- **Unit tests**: Pure logic, no I/O, fast feedback

Preference: Fast integration tests over slow E2E where possible. Use E2E for critical user journeys, integration tests for daemon behavior, unit tests for algorithms.
