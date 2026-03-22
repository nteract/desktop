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
    async with await client.create_notebook() as notebook:
        cell = await notebook.cells.create("print('hello')")
        result = await cell.run()
        print(result.stdout)   # "hello\n"

        # Sync reads from local CRDT
        print(cell.source)      # "print('hello')"
        print(cell.cell_type)   # "code"

        # Streaming execution (async iterator)
        cell2 = await notebook.cells.create("for i in range(3): print(i)")
        async for event in await cell2.stream():
            print(event)

        # Presence (cursor/selection sync)
        await notebook.presence.set_cursor(cell.id, line=0, column=5)

asyncio.run(main())
```

Other `Client` entry points:

```python
notebook = await client.open_notebook("/path/to/notebook.ipynb")
notebook = await client.join_notebook(notebook_id)

# List active notebooks
infos = await client.list_active_notebooks()  # list[NotebookInfo]
for info in infos:
    print(info.runtime_type, info.status, info.has_runtime)
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
# Sync reads from local CRDT via CellHandle properties
cell = notebook.cells.get_by_index(0)
print(cell.source, cell.cell_type, cell.outputs)
print(cell.tags, cell.source_hidden, cell.outputs_hidden)

# Search
matches = notebook.cells.find("import")

# Iterate all cells
for cell in notebook.cells:
    print(cell.id, cell.source[:40])

# Async mutations on CellHandle
await cell.set_source("new code")
await cell.splice(0, 0, "# inserted at top\n")
await cell.set_tags(["parameters"])
await cell.set_source_hidden(True)
await cell.clear_outputs()
await cell.delete()

# Runtime state (sync read)
print(notebook.runtime)

# Connected peers
print(notebook.peers)  # list of (peer_id, peer_label)
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

If `cell.run()` returns `Output(stream, stderr: "Failed to parse output: <hash>")`, the bindings are connecting to the wrong daemon. The blob store is per-daemon. Set `RUNTIMED_SOCKET_PATH` to the correct daemon socket.

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
