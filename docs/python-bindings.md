# Python Bindings (runtimed)

The `runtimed` Python package provides programmatic access to the notebook daemon. Use it to execute code, manage kernels, and interact with notebooks from Python scripts, agents, or automation workflows.

## Installation

```bash
# From PyPI (when published)
pip install runtimed

# From source
cd python/runtimed
uv run maturin develop
```

## Quick Start

### Synchronous API

```python
import runtimed

# Execute code with automatic kernel management
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
        print(result.stdout)  # "hello async\n"

asyncio.run(main())
```

## Session API

The `Session` class is the primary interface for executing code. Each session connects to a notebook room in the daemon.

### Creating a Session

```python
# Auto-generated notebook ID
session = runtimed.Session()

# Explicit notebook ID (allows sharing between sessions)
session = runtimed.Session(notebook_id="my-notebook")
```

### Kernel Lifecycle

```python
session.connect()                    # Connect to daemon (auto-called by start_kernel)
session.start_kernel()               # Launch Python kernel
session.start_kernel(kernel_type="deno")  # Launch Deno kernel
session.interrupt()                  # Interrupt running execution
session.shutdown_kernel()            # Stop the kernel
```

### Code Execution

```python
# Create cells in the document, then execute (document-first pattern)
cell_id = session.create_cell("x = 42")
result = session.execute_cell(cell_id)

# Execute another cell
cell_id2 = session.create_cell("print(x)")
result = session.execute_cell(cell_id2)

# Check results
print(result.success)         # True if no error
print(result.stdout)          # Captured stdout
print(result.stderr)          # Captured stderr
print(result.execution_count) # Execution counter
print(result.error)           # Error output if failed

# Queue execution without waiting (fire-and-forget)
session.queue_cell(cell_id)
# Poll for results later
cell = session.get_cell(cell_id)
```

### Document-First Execution

The session uses a document-first model where cells are stored in an automerge document. This enables multi-client synchronization.

```python
# Create a cell in the document
cell_id = session.create_cell("x = 10")

# Update cell source (full replacement, uses Myers diff internally)
session.set_source(cell_id, "x = 20")

# Append to cell source (direct CRDT insert, no diff — ideal for streaming)
session.append_source(cell_id, "\ny = 30")

# Execute by cell ID (daemon reads source from document)
result = session.execute_cell(cell_id)

# Read cell state
cell = session.get_cell(cell_id)
print(cell.source)           # "x = 20\ny = 30"
print(cell.execution_count)  # 1

# List all cells
cells = session.get_cells()

# Delete a cell
session.delete_cell(cell_id)
```

### Streaming Execution

Process outputs incrementally as they arrive from the kernel:

```python
cell_id = session.create_cell("for i in range(5): print(i)")
events = session.stream_execute(cell_id)

for event in events:
    if event.event_type == "execution_started":
        print(f"Started, count={event.execution_count}")
    elif event.event_type == "output":
        print(f"Output: {event.output}")
    elif event.event_type == "done":
        print("Done!")
```

### Context Manager

Sessions work as context managers for automatic cleanup:

```python
with runtimed.Session() as session:
    session.start_kernel()
    cell_id = session.create_cell("1 + 1")
    result = session.execute_cell(cell_id)
# Kernel automatically shut down on exit
```

### Properties

| Property | Type | Description |
|----------|------|-------------|
| `notebook_id` | `str` | Unique identifier for this notebook |
| `is_connected` | `bool` | Whether connected to daemon |
| `kernel_started` | `bool` | Whether kernel is running |
| `env_source` | `str \| None` | Environment source (e.g., "uv:prewarmed") |

## AsyncSession API

The `AsyncSession` class provides the same functionality as `Session` but with an async API for use in async Python code.

### Quick Start

```python
import asyncio
import runtimed

async def main():
    async with runtimed.AsyncSession() as session:
        await session.start_kernel()
        cell_id = await session.create_cell("print('hello async')")
        result = await session.execute_cell(cell_id)
        print(result.stdout)  # "hello async\n"

asyncio.run(main())
```

### Creating an AsyncSession

```python
# Auto-generated notebook ID
session = runtimed.AsyncSession()

# Explicit notebook ID (allows sharing between sessions)
session = runtimed.AsyncSession(notebook_id="my-notebook")
```

### Kernel Lifecycle

```python
await session.connect()              # Connect to daemon
await session.start_kernel()         # Launch Python kernel
await session.start_kernel(kernel_type="deno")  # Launch Deno kernel
await session.interrupt()            # Interrupt running execution
await session.shutdown_kernel()      # Stop the kernel
```

### Code Execution

```python
# Create and execute cells (document-first pattern)
cell_id = await session.create_cell("x = 42")
result = await session.execute_cell(cell_id)

cell_id2 = await session.create_cell("print(x)")
result = await session.execute_cell(cell_id2)

# Check results
print(result.success)         # True if no error
print(result.stdout)          # Captured stdout
print(result.stderr)          # Captured stderr

# Queue execution without waiting
await session.queue_cell(cell_id)
```

### Document-First Execution

```python
# Create a cell in the automerge document
cell_id = await session.create_cell("x = 10")

# Update cell source (full replacement, uses Myers diff internally)
await session.set_source(cell_id, "x = 20")

# Append to cell source (direct CRDT insert, no diff — ideal for streaming)
await session.append_source(cell_id, "\ny = 30")

# Execute by cell ID (daemon reads source from document)
result = await session.execute_cell(cell_id)

# Read cell state
cell = await session.get_cell(cell_id)
print(cell.source)  # "x = 20\ny = 30"

# List all cells
cells = await session.get_cells()

# Delete a cell
await session.delete_cell(cell_id)
```

### Streaming Execution

Process outputs incrementally as they arrive from the kernel:

```python
cell_id = await session.create_cell("for i in range(5): print(i)")
events = await session.stream_execute(cell_id)

for event in events:
    if event.event_type == "execution_started":
        print(f"Started, count={event.execution_count}")
    elif event.event_type == "output":
        print(f"Output: {event.output}")
    elif event.event_type == "done":
        print("Done!")
```

### Async Context Manager

AsyncSession works as an async context manager for automatic cleanup:

```python
async with runtimed.AsyncSession() as session:
    await session.start_kernel()
    cell_id = await session.create_cell("1 + 1")
    result = await session.execute_cell(cell_id)
# Kernel automatically shut down on exit
```

### Async Properties

Note that property accessors are async methods in AsyncSession:

```python
# These are coroutines, not properties
connected = await session.is_connected()     # bool
kernel_running = await session.kernel_started()  # bool
env = await session.env_source()             # str | None

# Only notebook_id is a sync property
notebook_id = session.notebook_id  # str
```

## DaemonClient API

The `DaemonClient` class provides low-level access to daemon operations.

```python
client = runtimed.DaemonClient()

# Health checks
client.ping()         # True if daemon responding
client.is_running()   # True if daemon process exists

# Pool status
stats = client.status()
# {
#   'uv_available': 2,
#   'conda_available': 0,
#   'uv_warming': 1,
#   'conda_warming': 0
# }

# Active notebook rooms
rooms = client.list_rooms()
# [
#   {
#     'notebook_id': 'my-notebook',
#     'active_peers': 2,
#     'has_kernel': True,
#     'kernel_type': 'python',
#     'kernel_status': 'idle',
#     'env_source': 'uv:prewarmed'
#   }
# ]

# Operations
client.flush_pool()   # Clear and rebuild environment pool
client.shutdown()     # Stop the daemon
```

## Result Types

### ExecutionResult

Returned by `execute_cell()`:

```python
cell_id = session.create_cell("print('hello')")
result = session.execute_cell(cell_id)

result.cell_id          # Cell that was executed
result.success          # True if no error
result.execution_count  # Execution counter value
result.outputs          # List of Output objects
result.stdout           # Combined stdout text
result.stderr           # Combined stderr text
result.display_data     # List of display_data/execute_result outputs
result.error            # First error output, or None
```

### ExecutionEvent

Returned by `stream_execute()`:

```python
events = session.stream_execute(cell_id)  # or: await session.stream_execute(cell_id)

for event in events:
    event.event_type      # "execution_started", "output", "done", "error"
    event.cell_id         # Cell this event is for
    event.output          # Output object (only for "output" events)
    event.execution_count # int (only for "execution_started" events)
    event.error_message   # str (only for "error" events)
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
    print(output.data)  # Dict[str, str] of MIME type -> content

    # For errors
    print(output.ename)      # Exception class name
    print(output.evalue)     # Exception message
    print(output.traceback)  # List of traceback lines
```

### Cell

Cell from the automerge document:

```python
cell = session.get_cell(cell_id)

cell.id              # Cell identifier
cell.cell_type       # "code", "markdown", or "raw"
cell.source          # Cell source content
cell.execution_count # Execution count if executed
```

## Multi-Client Scenarios

Two sessions with the same `notebook_id` share the same kernel and document:

```python
# Session 1 creates a cell and executes
s1 = runtimed.Session(notebook_id="shared")
s1.connect()
s1.start_kernel()
cell_id = s1.create_cell("x = 42")
s1.execute_cell(cell_id)

# Session 2 sees the cell and shares the kernel
s2 = runtimed.Session(notebook_id="shared")
s2.connect()
s2.start_kernel()  # Reuses existing kernel

cells = s2.get_cells()
assert any(c.id == cell_id for c in cells)

# Execute in s2, result visible to s1
cell_id2 = s2.create_cell("print(x)")
s2.execute_cell(cell_id2)  # Uses x=42 from s1's execution
```

This enables:
- Multiple Python processes sharing a notebook
- Python scripts interacting with notebooks open in the app
- Agent workflows with parallel execution

### Agentic Streaming

An agent can stream text into a cell while other clients see it in real-time:

```python
import asyncio
import runtimed

async def agent_writes_code():
    async with runtimed.AsyncSession(notebook_id="shared") as session:
        await session.start_kernel()

        # Create an empty cell
        cell_id = await session.create_cell("")

        # Stream tokens into the cell — each append is a CRDT op
        # that syncs to all connected clients in real-time
        for token in ["import ", "math\n", "print(", "math.pi", ")"]:
            await session.append_source(cell_id, token)
            await asyncio.sleep(0.05)  # Simulate LLM token delay

        # Execute the completed cell and stream outputs
        events = await session.stream_execute(cell_id)
        for event in events:
            if event.event_type == "output":
                print(f"Result: {event.output.text}")

asyncio.run(agent_writes_code())
```

## Error Handling

All errors raise `RuntimedError`:

```python
try:
    session.execute_cell("nonexistent-cell-id")
except runtimed.RuntimedError as e:
    print(f"Error: {e}")  # "Cell not found: nonexistent-cell-id"
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

## Sidecar (Rich Output Viewer)

The package also includes a sidecar launcher for rich output display:

```python
from runtimed import sidecar

# In a Jupyter kernel - auto-detects connection file
s = sidecar()

# In terminal IPython - creates IOPub bridge
s = sidecar()

# Explicit connection file
s = sidecar("/path/to/kernel-123.json")

# Check status
print(s.running)  # True if sidecar process is alive

# Cleanup
s.close()
```

The sidecar provides a GUI window that displays rich outputs (plots, HTML, images) from kernel execution.
