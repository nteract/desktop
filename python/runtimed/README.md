# runtimed

Python bindings for the [nteract](https://nteract.io) runtime daemon. Execute code, manage kernels, and interact with notebooks programmatically.

**[Download the nteract desktop app](https://nteract.io)** — it ships the runtimed daemon and gives you a visual interface for your notebooks.

> **Using runtimed with agents?** The [`nteract` MCP server](https://pypi.org/project/nteract/) is built on runtimed and provides a ready-made agentic interface for AI assistants. It's also a great example of how to use runtimed in practice.

## Installation

```bash
pip install runtimed
```

The stable release matches the [nteract desktop stable app](https://nteract.io). If you're running the nightly desktop app, install the pre-release to match: `pip install --pre runtimed` (or `uv pip install --prerelease allow runtimed`). The nightly build automatically discovers the nightly daemon socket.

`Client()` and the high-level Python API use `default_socket_path()` by default. That helper respects `RUNTIMED_SOCKET_PATH`, so exported test or MCP sockets take precedence over the package's default channel.

## Quick Start

> All examples use `await` — run them inside `asyncio.run(main())`, a Jupyter notebook, or a Python REPL with top-level await (e.g. `python -m asyncio`).

```python
import asyncio
import runtimed

async def main():
    client = runtimed.Client()
    notebook = await client.create_notebook()

    # Create and execute cells
    cell = await notebook.cells.create("print('hello')")
    result = await cell.run()
    print(result.stdout)  # "hello\n"

    # Read cell properties (sync — local CRDT replica)
    print(cell.source)      # "print('hello')"
    print(cell.cell_type)   # "code"

    # Edit cells
    await cell.set_source("x = 42")
    await cell.run()

    # Save the notebook
    path = await notebook.save_as("/tmp/my-notebook.ipynb")

asyncio.run(main())
```

## Features

- **Document-first model** with Automerge CRDT sync
- **Sync reads, async writes** — reads from local replica, writes sync to peers
- **Multi-client support** for shared notebooks
- **Rich output capture** (stdout, stderr, display_data, errors)

## API Overview

### Client

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
```

If you need to target a specific release channel instead of the current process default:

```python
import os
import runtimed

os.environ["RUNTIMED_SOCKET_PATH"] = runtimed.socket_path_for_channel("nightly")
client = runtimed.Client()
```

Use `default_socket_path()` for normal current-process behavior. Use `socket_path_for_channel("stable"|"nightly")` only for explicit channel targeting or cross-channel discovery because it intentionally ignores `RUNTIMED_SOCKET_PATH`.

### Notebook

```python
async with await client.create_notebook() as notebook:
    # Cells collection (sync reads, async writes)
    print(len(notebook.cells))
    for cell in notebook.cells:
        print(f"{cell.id[:8]}: {cell.source[:40]}")

    # Runtime state (sync read from local doc)
    print(notebook.runtime.kernel.status)

    # Runtime lifecycle
    await notebook.start(runtime="python")
    await notebook.restart()
    await notebook.interrupt()
    await notebook.save()
# Session closed automatically on exit
```

### Cells

```python
# Create cells
cell = await notebook.cells.create("import math")
cell = await notebook.cells.insert_at(0, "# Title", cell_type="markdown")

# Access cells
cell = notebook.cells.get_by_index(0)    # by position
cell = notebook.cells.get_by_id(cell_id) # by ID
matches = notebook.cells.find("import")  # search source

# Read properties (sync)
print(cell.source, cell.cell_type, cell.outputs)

# Mutate (async)
await cell.set_source("x = 2")
await cell.append("\ny = 3")
result = await cell.run()
await cell.delete()
```

## Requirements

- The runtimed daemon, which ships with the [nteract desktop app](https://nteract.io). For development, see the [nteract/desktop repo](https://github.com/nteract/desktop).
- Python 3.10+

## Documentation

See [docs/python-bindings.md](https://github.com/nteract/desktop/blob/main/docs/python-bindings.md) for full documentation.
