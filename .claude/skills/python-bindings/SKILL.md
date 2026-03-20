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
import runtimed

session = runtimed.Session()
session.connect()
session.start_kernel()

result = session.run("print('hello')")
print(result.stdout)   # "hello\n"
print(result.outputs)  # [Output(stream, stdout: "hello\n")]

# Rich output
result = session.run("from IPython.display import Image, display; display(Image(filename='photo.png'))")
for output in result.outputs:
    for mime, value in output.data.items():
        print(mime, type(value))
        # image/png  <class 'bytes'>       -- raw binary, NOT base64
        # text/llm+plain  <class 'str'>    -- synthesized blob URL
        # text/plain <class 'str'>
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

## Per-Cell Accessors

Prefer these O(1) methods over `get_cells()` (which materializes everything):

```python
source = session.get_cell_source(cell_id)    # just the source string
cell_type = session.get_cell_type(cell_id)   # "code" | "markdown" | "raw"
cell_ids = session.get_cell_ids()             # position-sorted IDs

# Full cell with outputs
cell = session.get_cell(cell_id)
print(cell.outputs, cell.position)

# Move a cell
session.move_cell("cell-id", after_cell_id="other-cell-id")

# Runtime state
state = await async_session.get_runtime_state()  # idle, busy, etc.
```

## Socket Path Configuration

**System daemon (default):**
```python
session = runtimed.Session()
session.connect()  # ~/Library/Caches/runt/runtimed.sock
```

**Worktree daemon (development):**
```bash
export RUNTIMED_SOCKET_PATH="$(./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')"
python your_script.py
```

## Cross-Session Output Visibility

The `Cell.outputs` field is from the Automerge document. Agents can see outputs from cells executed by other clients:

```python
s1 = runtimed.Session(notebook_id="shared")
s1.connect(); s1.start_kernel()
s1.run("x = 42")

s2 = runtimed.Session(notebook_id="shared")
s2.connect()
cells = s2.get_cells()
print(cells[0].outputs)  # Shows outputs from s1
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

If `session.run()` returns `Output(stream, stderr: "Failed to parse output: <hash>")`, the bindings are connecting to the wrong daemon. The blob store is per-daemon. Set `RUNTIMED_SOCKET_PATH` to the correct daemon socket.

### Empty outputs from get_cell()

If `session.run()` shows outputs but `session.get_cell()` returns `outputs=[]`:
1. Check socket path — daemon needs blob store access
2. Timing — outputs may not be written to Automerge yet. Try a small delay or re-fetch.

### Build not reflected

After changing Rust code in `crates/runtimed-py/`, rebuild into the correct venv:

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

Or if using Inkwell supervisor, call `supervisor_rebuild`.
