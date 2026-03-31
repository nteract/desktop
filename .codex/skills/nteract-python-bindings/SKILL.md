---
name: nteract-python-bindings
description: Build, rebuild, test, and debug the Python bindings and MCP server in the nteract desktop repo. Use when working in `crates/runtimed-py/**`, `python/runtimed/**`, `python/nteract/**`, or `python/gremlin/**`, especially for choosing the correct venv, running `maturin develop`, wiring tests to the right daemon socket, or validating MCP behavior after Rust changes.
---

# nteract Python Bindings

Use this skill when Python behavior depends on both Rust extension state and daemon selection.

## Workflow

1. Identify whether the task targets the workspace venv or the test venv.
2. Decide whether the task should follow the default nightly source flow or an explicit stable flow (`RUNT_BUILD_CHANNEL=stable`).
3. Rebuild bindings into the correct venv before trusting results from Python or MCP code.
4. Use the worktree daemon, not the system daemon, for daemon-backed tests.
5. Re-run the narrowest relevant test or command after rebuilding.

## Core Rules

- Use `.venv` at the repo root for `uv run nteract` and most day-to-day development. The MCP server is `runt mcp` (Rust); the Python `nteract` package is a convenience wrapper.
- Use `python/runtimed/.venv` for isolated pytest integration runs.
- Set `VIRTUAL_ENV` explicitly when running `maturin develop`; otherwise it is easy to rebuild into the wrong venv.
- If outputs, blobs, or notebook execution look wrong, verify `RUNTIMED_SOCKET_PATH` before assuming the bindings are broken.
- `default_socket_path()` is for the current process and respects `RUNTIMED_SOCKET_PATH`.
- `socket_path_for_channel("stable"|"nightly")` is for explicit channel targeting and ignores `RUNTIMED_SOCKET_PATH`.
- `nteract --stable` and `nteract --nightly` only seed `RUNTIMED_SOCKET_PATH` when it is unset; the flag also controls which app `show_notebook` launches.

## Quick Start

Read [references/bindings-workflows.md](references/bindings-workflows.md) for the exact rebuild and test commands.
