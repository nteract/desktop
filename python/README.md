# Python Packages

Development home for nteract Python packages.

| Package | Description |
|---------|-------------|
| `runtimed/` | Low-level Python bindings for the runtimed daemon (maturin/PyO3) |
| `nteract/` | MCP server for AI agents — composes runtimed primitives |

## Dev Setup (local runtimed + nteract)

To run the nteract MCP server against a locally-built runtimed (e.g., for testing presence, new protocol features):

```bash
cd runtimed

# 1. Build runtimed from Rust source
uv run --reinstall-package runtimed maturin develop

# 2. Install local nteract (editable, without re-resolving runtimed from PyPI)
uv pip install --no-deps -e ../nteract

# 3. Install nteract's other dependencies
uv pip install "mcp>=1.26.0" "httpx>=0.27.0,<1.0" "pydantic>=2.0"

# 4. Verify both packages are local
uv run python -c "import runtimed, nteract; print('ok')"
```

After this, the runtimed venv has both the local Rust build and the local nteract source.

## Running the MCP Server (Dev)

```bash
# Find your dev daemon socket (from repo root)
RUNTIMED_DEV=1 ./target/debug/runt daemon status

# Run the MCP server (from python/runtimed/)
RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock \
    uv run --no-sync nteract
```

For Zed MCP config:

```json
{
  "command": "uv",
  "args": ["run", "--no-sync", "nteract"],
  "env": {
    "RUNTIMED_SOCKET_PATH": "/Users/<you>/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock"
  },
  "working_directory": "/path/to/desktop/python/runtimed"
}
```

The `--no-sync` flag prevents uv from re-resolving dependencies and overwriting the local runtimed build.

## Running Demos

```bash
# From python/runtimed/
RUNTIMED_SOCKET_PATH=... uv run python demos/presence_cursor.py <notebook_id>
```

## Rebuilding After Rust Changes

If you change code in `crates/runtimed-py/` or `crates/runtimed/`:

```bash
cd python/runtimed
uv run --reinstall-package runtimed maturin develop
```