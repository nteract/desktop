# runtimed

Python toolkit for Jupyter runtimes, powered by runtimed Rust binaries. Execute code, manage kernels, and interact with notebooks programmatically.

## Installation

```bash
pip install runtimed
```

> **Note:** The 2.0 release is currently in pre-release. Use `pip install --pre runtimed` (or `uv pip install --prerelease allow runtimed`) to get the latest 2.x build matching the nteract desktop nightly.

## Quick Start

> All examples use `await` — run them inside `asyncio.run(main())`, a Jupyter notebook, or a Python REPL with top-level await (e.g. `python -m asyncio`).

```python
import asyncio
import runtimed

async def main():
    client = runtimed.Client()
    notebook = await client.create()

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
    path = await notebook.save("/tmp/my-notebook.ipynb")

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
    print(f"{info.name} [{info.kernel_status}] ({info.active_peers} peers)")

# Open, create, or join notebooks
notebook = await client.open("/path/to/notebook.ipynb")
notebook = await client.create(runtime="python")
notebook = await client.join(notebook_id)
```

### Notebook

```python
async with await client.create() as notebook:
    # Cells collection (sync reads, async writes)
    print(len(notebook.cells))
    for cell in notebook.cells:
        print(f"{cell.id[:8]}: {cell.source[:40]}")

    # Runtime state (sync read from local doc)
    print(notebook.runtime.kernel.status)

    # Kernel lifecycle
    await notebook.start_kernel(kernel_type="python")
    await notebook.restart_kernel()
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

- runtimed daemon running (see [CLAUDE.md](../../CLAUDE.md) — use `cargo xtask dev-daemon` for development or `cargo xtask install-daemon` for the system service)
- Python 3.10+

## Documentation

See [docs/python-bindings.md](https://github.com/nteract/desktop/blob/main/docs/python-bindings.md) for full documentation.
