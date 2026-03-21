---
name: python-bindings
description: Build, test, and develop Python bindings (runtimed-py, nteract MCP server). Use when working on Python code, maturin builds, or the MCP server.
---

# Python Bindings & MCP Server

## Two Venvs

| Venv | Path | Purpose | Used by |
|------|------|---------|---------|
| Workspace venv | `.venv` (repo root) | Day-to-day dev, MCP server | `uv run nteract`, gremlin agent |
| Test venv | `python/runtimed/.venv` | Isolated pytest runs | `pytest` integration tests |

## Installation

### Into workspace venv (most common)

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

This is what `supervisor_rebuild` does automatically.

### Into test venv (for pytest)

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../python/runtimed/.venv uv run --directory ../../python/runtimed maturin develop
```

### Common mistake

Running `maturin develop` without `VIRTUAL_ENV` installs the `.so` into whichever venv `uv run` resolves (`python/runtimed/.venv`). The MCP server runs from `.venv` (repo root) and will never see it. Always set `VIRTUAL_ENV` explicitly.

## Basic Usage

```python
import asyncio
import runtimed

async def main():
    client = runtimed.Client()
    async with await client.create() as notebook:
        cell = await notebook.cells.create("print('hello')")
        result = await cell.run()
        print(result.stdout)   # "hello\n"

        # Sync reads from local CRDT
        print(cell.source)      # "print('hello')"
        print(cell.cell_type)   # "code"

asyncio.run(main())
```

For the native session API (streaming, presence, metadata), use `NativeAsyncClient`:

```python
native_client = runtimed.NativeAsyncClient()
session = await native_client.create_notebook()
result = await session.run("print('hello')")
```

## Output.data Typing

`Output.data` is `dict[str, str | bytes | dict]`. Value type depends on MIME:

| MIME category | Example | Python type | Notes |
|---------------|---------|-------------|-------|
| Binary image | `image/png`, `image/jpeg` | `bytes` | Raw binary (not base64) |
| JSON | `application/json` | `dict` | Parsed JSON object |
| Text | `text/plain`, `text/html` | `str` | UTF-8 string |
| LLM hint | `text/llm+plain` | `str` | Synthesized blob URL for LLM consumers |

## text/llm+plain Synthesis

When an output contains a binary image MIME type, the daemon synthesizes a `text/llm+plain` entry combining text/plain, image metadata, and blob URL. Lets LLMs reference images without decoding binary data.

## High-Level Cell Access

The `Notebook.cells` collection provides sync reads and async writes:

```python
# Sync reads from local CRDT
cell = notebook.cells.get_by_index(0)
print(cell.source, cell.cell_type, cell.outputs)

# Search
matches = notebook.cells.find("import")

# Runtime state
print(notebook.runtime.kernel.status)  # sync read
```

For the native session API, per-cell accessors are also available:

```python
source = session.get_cell_source(cell_id)    # just the source string
cell_type = session.get_cell_type(cell_id)   # "code" | "markdown" | "raw"
cell_ids = session.get_cell_ids()             # position-sorted IDs
```

## Socket Path Configuration

**System daemon (default):**
```python
client = runtimed.Client()  # ~/Library/Caches/runt/runtimed.sock
```

**Worktree daemon (development):**
```bash
export RUNTIMED_SOCKET_PATH="$(./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')"
python your_script.py
```

## Running Integration Tests

```bash
# Get socket path from dev daemon
RUNTIMED_SOCKET_PATH="$(./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')" \
  python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v

# Unit tests (no daemon needed)
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v

# Skip integration tests
SKIP_INTEGRATION_TESTS=1 python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/ -v
```

## MCP Server

The nteract MCP server (`python/nteract/`) provides programmatic notebook interaction for AI agents.

```bash
# Run directly (after uv sync + maturin develop)
uv run nteract

# Via Inkwell supervisor (recommended, handles lifecycle)
cargo xtask run-mcp
```

Three packages are workspace members:

| Package | Path | Purpose |
|---------|------|---------|
| `runtimed` | `python/runtimed` | Python bindings (PyO3/maturin) |
| `nteract` | `python/nteract` | MCP server |
| `gremlin` | `python/gremlin` | Autonomous notebook stress tester |

## Troubleshooting

### Wrong daemon

If `notebook.run()` returns `Output(stream, stderr: "Failed to parse output: <hash>")`, the bindings are connecting to the wrong daemon. The blob store is per-daemon. Set `RUNTIMED_SOCKET_PATH` to the correct daemon socket.

### Empty outputs from cell.outputs

If `cell.run()` shows outputs but `cell.outputs` returns `[]`:
1. Check socket path — daemon needs blob store access
2. Timing — outputs may not be written to Automerge yet. Try a small delay or re-fetch.

### Build not reflected

After changing Rust code in `crates/runtimed-py/`, rebuild into the correct venv:

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

Or if using Inkwell supervisor, call `supervisor_rebuild`.
