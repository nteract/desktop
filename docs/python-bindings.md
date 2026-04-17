# Python Bindings (runtimed)

The `runtimed` Python package provides programmatic access to the notebook daemon. Use it to execute code, manage kernels, and interact with notebooks from Python scripts, agents, or automation workflows.

> **Looking for an MCP server?** `runt mcp` (Rust, shipped as a sidecar of the desktop app) is the canonical MCP path — you do not need Python or uv for it. The Python `nteract` package that launches `runt mcp` still ships, but these bindings are for programmatic use, not MCP integration.

## Installation

```bash
# From PyPI
pip install runtimed

# From source (inside the python/runtimed package directory)
cd python/runtimed
uv run maturin develop
```

### Building from `crates/runtimed-py`

When working on the Rust bindings directly, run maturin from the crate directory and point `VIRTUAL_ENV` at the workspace venv:

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

### Two virtual environments

The repo uses two venvs:

| Venv | Path (from repo root) | Purpose |
|------|-----------------------|---------|
| Workspace | `.venv` | Main development venv. The native extension is installed here via `maturin develop`. |
| Test | `python/runtimed/.venv` | Isolated venv for `pytest` / CI. Created automatically by `uv run` inside `python/runtimed/`. |

`VIRTUAL_ENV=../../.venv` in the maturin command above ensures the compiled `.so` lands in the workspace venv rather than the test venv.

## Quick Start

> All examples use `await` — run them inside `asyncio.run(main())`, a Jupyter notebook, or `python -m asyncio`.

```python
import asyncio
import runtimed

async def main():
    client = runtimed.Client()
    async with await client.create_notebook() as notebook:
        cell = await notebook.cells.create("print('hello')")
        result = await cell.run()
        print(result.stdout)  # "hello\n"

asyncio.run(main())
```

## Client

`runtimed.Client` is the primary entry point. It wraps the daemon connection and returns `Notebook` objects.

```python
client = runtimed.Client()

# Discover active notebooks
notebooks = await client.list_active_notebooks()
for info in notebooks:
    print(f"{info.name} [{info.status}] ({info.active_peers} peers)")

# Open, create, or join notebooks
notebook = await client.open_notebook("/path/to/notebook.ipynb")
notebook = await client.create_notebook(runtime="python")
notebook = await client.join_notebook(notebook_id)

# Health checks
await client.ping()        # True if daemon responding
await client.is_running()  # True if daemon process exists

# Pool status
stats = await client.status()
# {'uv_available': 2, 'conda_available': 0, ...}

# Operations
await client.flush_pool()  # Clear and rebuild environment pool
await client.shutdown()    # Stop the daemon
```

## Notebook

A `Notebook` wraps a connected session. Properties are **sync reads** from the local Automerge replica. Methods are **async writes** that sync to peers.

```python
async with await client.create_notebook() as notebook:
    print(notebook.notebook_id)

    # Cells collection (sync reads, async writes)
    print(len(notebook.cells))
    for cell in notebook.cells:
        print(f"{cell.id[:8]}: {cell.source[:40]}")

    # Runtime state (sync read from local doc)
    print(notebook.runtime.kernel.status)
    print(notebook.peers)

    # Runtime lifecycle
    await notebook.start(runtime="python")
    await notebook.start(runtime="deno")
    await notebook.restart()
    await notebook.interrupt()
    await notebook.stop_runtime()

    # Save
    path = await notebook.save()
    path = await notebook.save_as("/tmp/copy.ipynb")

    # Execute code
    cell = await notebook.cells.create("print('hello')")
    result = await cell.run()
# Session closed automatically on exit
```

### CellCollection

Access via `notebook.cells`. Sync reads, async mutations.

```python
# Create cells
cell = await notebook.cells.create("import math")
cell = await notebook.cells.insert_at(0, "# Title", cell_type="markdown")

# Access cells (sync)
cell = notebook.cells.get_by_index(0)     # by position (supports negative)
cell = notebook.cells.get_by_index(-1)    # last cell
cell = notebook.cells.get_by_id(cell_id)  # by ID
cell = notebook.cells[cell_id]            # sugar for get_by_id
matches = notebook.cells.find("import")   # search source text

# Iteration (sync)
for cell in notebook.cells:
    print(cell.source)

len(notebook.cells)          # cell count
"cell-id" in notebook.cells  # membership test
notebook.cells.ids           # all cell IDs in order
```

### CellHandle

A live reference to a cell. Properties read from the local CRDT. Methods write async.

```python
# Sync reads
cell.id               # str
cell.source           # str
cell.cell_type        # "code", "markdown", or "raw"
cell.outputs          # list[Output]
cell.execution_count  # int | None
cell.metadata         # dict
cell.snapshot()       # full Cell object

# Async mutations (return self for chaining, except run/delete)
await cell.set_source("x = 2")
await cell.append("\ny = 3")
await cell.splice(index=0, delete_count=5, text="new ")
await cell.set_type("markdown")
await cell.move_after(other_cell)
await cell.clear_outputs()
await cell.delete()

# Execution
result = await cell.run(timeout_secs=60.0)  # returns ExecutionResult
await cell.queue()  # fire-and-forget
```

## Result Types

### ExecutionResult

Returned by `cell.run()`:

```python
result = await cell.run()

result.cell_id          # Cell that was executed
result.success          # True if no error
result.execution_count  # Execution counter value
result.outputs          # List of Output objects
result.stdout           # Combined stdout text
result.stderr           # Combined stderr text
result.display_data     # List of display_data/execute_result outputs
result.error            # First error output, or None
```

### Output

Individual outputs from execution:

```python
for output in result.outputs:
    print(output.output_type)  # "stream", "display_data", "execute_result", "error"

    # For streams
    print(output.name)  # "stdout" or "stderr"
    print(output.text)  # The text content

    # For display_data/execute_result
    print(output.data)  # Dict[str, str | bytes | dict] of MIME type -> content

    # For errors
    print(output.ename)      # Exception class name
    print(output.evalue)     # Exception message
    print(output.traceback)  # List of traceback lines
```

#### `Output.data` value types

Values in the `output.data` dict are typed by MIME category — not always `str`:

| MIME category | Python type | Examples |
|---------------|-------------|----------|
| Text | `str` | `text/plain`, `text/html`, `text/markdown`, `image/svg+xml`, `application/javascript` |
| Binary | `bytes` | `image/png`, `image/jpeg`, `audio/*`, `video/*` |
| JSON | `dict` (or `list`) | `application/json`, `application/vnd.dataresource+json`, any `*+json` |

Binary MIME types are returned as **raw bytes** — no base64 encoding.

### RuntimeState

Sync read from the local RuntimeStateDoc:

```python
rs = notebook.runtime
rs.kernel.status      # "not_started", "starting", "idle", "busy", "error", "shutdown"
rs.kernel.language    # "python", "typescript", etc.
rs.kernel.name        # Kernel display name
rs.kernel.env_source  # "uv:prewarmed", "conda:inline", etc.
rs.queue.executing    # Cell ID currently executing, or None
rs.queue.queued       # List of queued cell IDs
rs.env.in_sync        # Whether deps match the kernel environment
rs.env.added          # Packages in metadata but not in kernel
rs.env.removed        # Packages in kernel but not in metadata
rs.last_saved         # ISO timestamp of last save, or None
```

## Multi-Client Scenarios

Multiple clients joining the same notebook share the kernel and document:

```python
async def multi_client_demo():
    client = runtimed.Client()

    # Client 1 creates a notebook and executes code
    nb1 = await client.create_notebook()
    cell = await nb1.cells.create("x = 42")
    await cell.run()

    # Client 2 joins the same notebook, sees the cell, shares the kernel
    nb2 = await client.join_notebook(nb1.notebook_id)
    print(len(nb2.cells))  # 1 — synced from nb1

    cell2 = await nb2.cells.create("print(x)")
    result = await cell2.run()
    print(result.stdout)  # "42\n" — shared kernel state
```

## Streaming Execution & Presence

The wrapper API covers streaming execution, presence, and all other
session-level features — no need to drop down to native types:

```python
client = runtimed.Client()
nb = await client.create_notebook()
await nb.start()

cell = await nb.cells.create("print('hello')")

# Streaming execution
async for event in await cell.stream():
    if event.event_type == "output":
        print(event.output.text)

# Presence
await nb.presence.set_cursor(cell.id, line=0, column=5)
peers = nb.peers  # sync read — list of (peer_id, label) tuples

await nb.disconnect()
```

### Internal: Native Session API

> **Note:** `NativeAsyncClient` and `AsyncSession` are internal types not
> included in the public `__all__`. The wrapper API above covers their
> functionality. If you genuinely need the raw session (e.g. for methods
> not yet wrapped), import them explicitly:
>
> ```python
> from runtimed._internals import NativeAsyncClient, AsyncSession
> ```

## Error Handling

All errors raise `RuntimedError`:

```python
try:
    await cell.run()
except runtimed.RuntimedError as e:
    print(f"Error: {e}")
```

Common error scenarios:
- Connection to daemon fails
- Kernel not started before execution
- Cell not found
- Execution timeout
- Kernel errors

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUNTIMED_WORKSPACE_PATH` | Use dev daemon for this worktree |
| `RUNTIMED_SOCKET_PATH` | Override daemon socket path |
