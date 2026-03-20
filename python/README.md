# Python Packages

Development home for nteract Python packages. The UV workspace root (`pyproject.toml` and `.venv`) lives at the **repo root**, with three workspace members:

| Package | Description |
|---------|-------------|
| `python/runtimed/` | Low-level Python bindings for the runtimed daemon (maturin/PyO3) |
| `python/nteract/` | MCP server for AI agents — composes runtimed primitives |
| `python/gremlin/` | Autonomous notebook agent for stress testing |

## Dev Setup

The virtual environment lives at the repo root (`.venv`), not inside `python/`.

```bash
# From the repo root — creates .venv and installs all workspace members
uv sync
```

### Building runtimed from Rust source

The Rust bindings live in `crates/runtimed-py/`. To build them into the repo-root `.venv`:

```bash
cd crates/runtimed-py && VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

Verify everything is wired up:

```bash
# From the repo root
uv run python -c "import runtimed, nteract; print('ok')"
```

## Running the MCP Server (Dev)

```bash
# Find your dev daemon socket (from repo root)
RUNTIMED_DEV=1 ./target/debug/runt daemon status

# Run the MCP server (from repo root)
RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock \
    uv run nteract
```

`uv run --no-sync nteract` also works from the repo root if you want to skip dependency resolution.

For Zed MCP config:

```json
{
  "command": "uv",
  "args": ["run", "--no-sync", "nteract"],
  "env": {
    "RUNTIMED_SOCKET_PATH": "/Users/<you>/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock"
  },
  "working_directory": "/path/to/desktop"
}
```

## Running Demos

```bash
# From the repo root
RUNTIMED_SOCKET_PATH=... uv run python python/runtimed/demos/presence_cursor.py <notebook_id>
```

## Rebuilding After Rust Changes

If you change code in `crates/runtimed-py/` or `crates/runtimed/`:

```bash
cd crates/runtimed-py && VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```
