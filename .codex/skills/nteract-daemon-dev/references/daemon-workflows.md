# Daemon Workflows

## Manual setup

Run all manual daemon commands from the repo root:

```bash
export RUNTIMED_DEV=1
export RUNTIMED_WORKSPACE_PATH="$(pwd)"
```

Source builds default to the nightly channel. To validate stable-specific names or paths from source, prefix the relevant commands with `RUNT_BUILD_CHANNEL=stable`.

Start the worktree daemon:

```bash
cargo xtask dev-daemon
```

Force the stable flow from source:

```bash
RUNT_BUILD_CHANNEL=stable cargo xtask dev-daemon
RUNT_BUILD_CHANNEL=stable cargo xtask notebook
RUNT_BUILD_CHANNEL=stable cargo xtask run-mcp
```

Check status or logs:

```bash
./target/debug/runt daemon status
./target/debug/runt daemon logs -f
./target/debug/runt ps
./target/debug/runt notebooks
```

## Derive the socket path

Use this before Python integration tests, `uv run nteract`, or anything else that needs the daemon socket explicitly:

```bash
export RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])'
)"
```

Use `default_socket_path()` when you want code to honor that exported socket. Use `socket_path_for_channel("stable"|"nightly")` only for explicit cross-channel discovery.

## When to start your own daemon

Start or restart the worktree daemon when:

- You changed `crates/runtimed/**`
- You are reviewing or debugging notebook sync behavior that depends on the daemon
- You are running daemon-backed Python tests
- You are verifying MCP server behavior against local Rust changes
- You are comparing behavior across worktrees and need isolation

## Prefer supervisor tools when available

If the MCP supervisor is available, prefer:

- `supervisor_restart(target="daemon")`
- `supervisor_status`
- `supervisor_logs`

These avoid manual env-var mistakes.

## Safety rules

- Use `./target/debug/runt daemon stop` instead of `pkill` or `killall`
- Do not point tests at the system daemon by accident
- Do not launch `cargo xtask notebook` from an agent terminal
