# runtimed

Python toolkit for Jupyter runtimes, powered by runtimed Rust binaries. Execute code, manage kernels, and interact with notebooks programmatically.

## Installation

```bash
pip install runtimed
```

> **Note:** The 2.0 release is currently in pre-release. Use `pip install --pre runtimed` (or `uv pip install --prerelease allow runtimed`) to get the latest 2.x build matching the nteract desktop nightly.

## Quick Start

### Synchronous API

```python
import runtimed

with runtimed.Session() as session:
    session.start_kernel()
    result = session.run("print('hello')")
    print(result.stdout)  # "hello\n"
```

### Async API

```python
import asyncio
import runtimed

async def main():
    async with runtimed.AsyncSession() as session:
        await session.start_kernel()
        result = await session.run("print('hello async')")
        print(result.stdout)

asyncio.run(main())
```

## Features

- **Code execution** via daemon-managed kernels
- **Sync and async APIs** for flexibility
- **Document-first model** with automerge sync
- **Multi-client support** for shared notebooks
- **Rich output capture** (stdout, stderr, display_data, errors)

## Session API

```python
session = runtimed.Session(notebook_id="my-notebook")
session.start_kernel()

# Simple execution
result = session.run("x = 42")

# Document-first pattern (for fine-grained control)
cell_id = session.create_cell("print(x)")
result = session.execute_cell(cell_id)

# Inspect results
print(result.success)
print(result.stdout)
print(result.error)

# Save the notebook to disk
path = session.save()                       # Save to current path
path = session.save(path="/tmp/copy.ipynb")  # Save-as

# Reorder cells
new_position = session.move_cell(cell_id, after_cell_id="other-id")
```

## AsyncSession API

```python
async with runtimed.AsyncSession(notebook_id="my-notebook") as session:
    await session.start_kernel()
    result = await session.run("x = 42")

    # Or document-first pattern
    cell_id = await session.create_cell("print(x)")
    result = await session.execute_cell(cell_id)

    # Save the notebook to disk
    path = await session.save()                       # Save to current path
    path = await session.save(path="/tmp/copy.ipynb")  # Save-as

    # Reorder cells
    new_position = await session.move_cell(cell_id, after_cell_id="other-id")
```

## DaemonClient API

```python
client = runtimed.DaemonClient()
client.ping()        # Health check
client.status()      # Pool statistics
client.list_rooms()  # Active notebooks
```

## Requirements

- runtimed daemon running (see [CLAUDE.md](../../CLAUDE.md) — use `cargo xtask dev-daemon` for development or `cargo xtask install-daemon` for the system service)
- Python 3.10+

## Documentation

See [docs/python-bindings.md](https://github.com/nteract/desktop/blob/main/docs/python-bindings.md) for full documentation.
