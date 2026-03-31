# Python Bindings Workflows

## Venv split

Use these venvs intentionally:

- `.venv` at the repo root: workspace venv for `uv run nteract`, `gremlin`, and general development
- `python/runtimed/.venv`: test venv for pytest integration runs

Source builds default to the nightly channel. Prefix `cargo xtask ...` with `RUNT_BUILD_CHANNEL=stable` only when you are intentionally validating stable daemon/app behavior.

## Rebuild into the workspace venv

Use this for MCP server work and most local development:

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

## Rebuild into the test venv

Use this before daemon-backed pytest runs:

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../python/runtimed/.venv uv run --directory ../../python/runtimed maturin develop
```

## Run tests

Unit-only:

```bash
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v
```

Daemon-backed integration:

```bash
RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])'
)" \
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v
```

## Run the MCP server

The MCP server is `runt mcp` (Rust, shipped with the desktop app). For development:

```bash
# Using the built binary directly
./target/debug/runt mcp

# Or via the Python wrapper (finds and launches runt mcp)
uv run nteract
```

If the MCP supervisor is available, prefer `cargo xtask run-mcp` or the supervisor tools instead of a manual launch.

Use `default_socket_path()` for current-process resolution. Use `socket_path_for_channel("stable"|"nightly")` only when you need an explicit channel path that ignores `RUNTIMED_SOCKET_PATH`.

## Common failure mode

If `maturin develop` ran successfully but behavior did not change, you likely rebuilt into the wrong venv. Check `VIRTUAL_ENV` first.
