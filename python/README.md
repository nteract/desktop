# Python Workspace

Development workspace for nteract Python packages. Uses [uv workspaces](https://docs.astral.sh/uv/concepts/workspaces/) so `nteract` resolves `runtimed` from the local Rust build instead of PyPI.

## Packages

| Package | Description |
|---------|-------------|
| `runtimed/` | Low-level Python bindings for the runtimed daemon (maturin/PyO3) |
| `nteract/` | MCP server for AI agents — composes runtimed primitives |

## Dev Setup

From this directory (`python/`):

```bash
# 1. Build runtimed from Rust source into the workspace venv
uv run maturin develop --manifest-path ../crates/runtimed-py/Cargo.toml

# 2. Install nteract (editable) with its dependencies
uv pip install -e nteract

# 3. Verify both packages are local
uv run python -c "import runtimed, nteract; print('ok')"
```

After step 2, `nteract` uses the locally-built `runtimed` (with presence support, latest protocol changes, etc.) instead of the PyPI release.

## Running the MCP Server (Dev)

Point at your dev daemon socket:

```bash
# Find your dev daemon socket (from repo root)
RUNTIMED_DEV=1 ../../target/debug/runt daemon status

# Run the MCP server
RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock \
    uv run nteract
```

For Zed MCP config:

```json
{
  "command": "uv",
  "args": ["run", "--no-sync", "nteract"],
  "env": {
    "RUNTIMED_SOCKET_PATH": "/Users/<you>/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock"
  },
  "working_directory": "/path/to/desktop/python"
}
```

The `--no-sync` flag prevents uv from re-resolving dependencies and overwriting the local runtimed build.

## Running Demos

```bash
# From this directory
RUNTIMED_SOCKET_PATH=... uv run python runtimed/demos/presence_cursor.py <notebook_id>
```

## Rebuilding After Rust Changes

If you change code in `crates/runtimed-py/` or `crates/runtimed/`, rebuild:

```bash
uv run maturin develop --manifest-path ../crates/runtimed-py/Cargo.toml
```

This recompiles the Rust code and reinstalls the Python package in the workspace venv.