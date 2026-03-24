---
name: nteract-daemon-dev
description: Work with the per-worktree runtimed daemon in the nteract desktop repo. Use when changing `crates/runtimed/**`, debugging daemon-backed notebook behavior, deriving `RUNTIMED_SOCKET_PATH`, checking daemon logs/status, running daemon-backed tests or reviews, or deciding whether to use `supervisor_*` tools versus manual `cargo xtask dev-daemon` commands.
---

# nteract Daemon Dev

Use this skill to avoid talking to the wrong daemon and to keep daemon-backed verification tied to the current worktree.

## Workflow

1. Prefer `supervisor_*` tools when they are available.
2. Decide whether you are validating the default nightly source flow or an explicit stable flow. Source builds are nightly unless `RUNT_BUILD_CHANNEL=stable`.
3. Otherwise, treat the worktree daemon as mandatory for daemon-backed verification.
4. Export `RUNTIMED_DEV=1` and `RUNTIMED_WORKSPACE_PATH="$(pwd)"` before any manual `runt` command.
5. Start or restart the daemon before validating changes in `crates/runtimed/**`, notebook sync paths, or Python integration flows.
6. Derive `RUNTIMED_SOCKET_PATH` from `./target/debug/runt daemon status --json` before running Python or cross-implementation tests.

## Guardrails

- Never use `pkill`, `killall`, or other system-wide process killers for `runtimed`.
- Never assume the system daemon is correct for a repo worktree.
- Never run the notebook GUI from an agent terminal; let the human launch it.
- If a test or script depends on notebook execution, blob resolution, or MCP server behavior, confirm it is pointed at the worktree daemon first.
- Use `default_socket_path()` for the current process. Reach for `socket_path_for_channel(...)` only when you intentionally need stable/nightly discovery that ignores `RUNTIMED_SOCKET_PATH`.

## Quick Start

If you have supervisor tools, use them for daemon lifecycle and logs.

If you do not, read [references/daemon-workflows.md](references/daemon-workflows.md) and follow the manual command sequence there.
