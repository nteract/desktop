# Release Readiness — v2.0.2 cycle

Last updated: 2026-03-20

Target: Monday/Tuesday 2026-03-24/25

---

## Bugs Found (this session)

### 🔴 Kernel launch fails from Python bindings — "Disconnected from sync task"

- **Severity**: High — blocks any programmatic kernel start via `runtimed` Python API
- **Repro**: `session.start_kernel()` or `session.run()` on any notebook (fresh or existing)
- **Error**: `runtimed.RuntimedError: Disconnected from sync task`
- **Scope**: Affects `Session` and `AsyncSession`. Daemon is healthy (`ping()` works, pool has 5 envs). The session connects and reads cells fine — only kernel launch fails.
- **Likely on main too**: User suspects this predates PR #990. Needs verification on `main`.
- **Frontend unaffected**: Tauri app launches kernels via a different code path (direct daemon IPC, not Python bindings).
- **Action**: Reproduce on `main`. If confirmed, file issue. If regression from #990, bisect.

### 🟡 `ImageContent` not processed as vision by Claude Code CLI

- **Severity**: Medium — agents can't "see" images in tool results
- **Filed**: #991
- **Workaround**: Blob store file paths + `Read` tool, or `text/llm+plain` descriptions (both implemented in #990)
- **Not a release blocker**: Text descriptions work. Vision is a nice-to-have.

### 🟡 `get_all_cells(format='rich')` could crash agents with large images

- **Severity**: Medium — agent SDK fatal error, not a daemon/app crash
- **Fixed in**: #990 — images replaced with `[image: mime/type]` placeholders in bulk view
- **Also fixed**: Image resizing via `_fit_image_for_llm()` for individual cell responses

---

## Changes Landing in #990

All of these need smoke testing before release:

### Python workspace root moved to repo root

- [ ] `uv sync` works from repo root (clean checkout)
- [ ] `uv run nteract` launches MCP server
- [ ] `uv run gremlin <id>` works on macOS
- [ ] `cargo xtask lint --fix` passes
- [ ] `cargo xtask run-mcp` works (supervisor finds `.venv`)
- [ ] `supervisor_rebuild` builds into root `.venv`
- [ ] CI passes (Linux — gremlin skipped via platform marker)
- [ ] Notebooks in `notebooks/` directory open and run (pyproject.toml detection)

### `AsyncSession.get_runtime_state()`

- [ ] Accessible from Python: `await session.get_runtime_state()`
- [ ] Returns `PyRuntimeState` with kernel, queue, env fields
- [ ] Sync `Session.get_runtime_state()` still works (refactored to shared `session_core`)

### Typed `Output.data`

- [ ] `output.data["image/png"]` returns `bytes` (raw PNG, not base64)
- [ ] `output.data["text/plain"]` returns `str`
- [ ] `output.data["application/json"]` returns `dict`
- [ ] `output.data["image/svg+xml"]` returns `str` (XML text)
- [ ] `text/llm+plain` synthesized for image outputs with blob URL
- [ ] nteract MCP server handles all three types correctly
- [ ] Existing tests pass (`python/runtimed/tests/`, `python/nteract/tests/`)

### nteract MCP server image handling

- [ ] `_fit_image_for_llm()` resizes large PNGs without crashing
- [ ] `get_all_cells(format='rich')` returns text placeholders for images
- [ ] `get_cell` returns resized `ImageContent` for individual cells
- [ ] `execute_cell` with plot output doesn't crash the agent

---

## Pre-release Checklist

### Build & Package

- [ ] `cargo xtask build-app` succeeds
- [ ] App launches, connects to daemon
- [ ] Notebook opens, kernel starts, cells execute
- [ ] Matplotlib plots render in output area
- [ ] Save/load round-trip preserves outputs (blob store manifests)

### MCP Server

- [ ] `cargo xtask run-mcp --print-config` outputs valid JSON
- [ ] MCP server responds to tool calls from Zed/Claude Desktop
- [ ] `join_notebook` → `get_all_cells` → `execute_cell` flow works
- [ ] Agent can create, edit, delete, move cells
- [ ] `add_dependency` + `sync_environment` works for UV inline deps

### Python Packages (PyPI)

- [ ] `runtimed` wheel builds for macOS ARM, macOS x86, Linux x86
- [ ] `nteract` pure-Python wheel builds
- [ ] Version bumps if needed
- [ ] `pip install runtimed nteract` works in a clean venv

### Daemon

- [ ] Dev daemon starts: `cargo xtask dev-daemon`
- [ ] Production daemon installs: `cargo xtask install-daemon`
- [ ] Pool prewarming works (UV and Conda)
- [ ] Blob server starts and serves images
- [ ] Autosave writes valid `.ipynb` files

---

## Known Limitations (not blockers)

- `ImageContent` in MCP tool results doesn't enable Claude vision (#991)
- `claude-agent-sdk` is macOS ARM only — gremlin doesn't work on Linux/Intel
- `source_hidden` is UI-only — MCP `get_cell` still returns full source
- Gremlin uses `query()` (no interrupt support); `ClaudeSDKClient` would be better for long runs