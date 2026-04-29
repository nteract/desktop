# Agent Guide

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This file is a map, not the full handbook. Keep repo-specific facts here only
when they prevent wrong turns. Detailed setup, architecture, and subsystem
workflow docs live under `contributing/`, `docs/`, and `.codex/skills/`.
Claude-specific rules and skills also live under `.claude/`.

Start each session with:

```bash
cargo xtask help
```

## First Places To Look

Use the repo-local Codex skills when they match the task:

| Task | Skill |
|------|-------|
| Daemon lifecycle, socket setup, daemon-backed verification | `.codex/skills/nteract-daemon-dev/` |
| Python bindings, MCP server work, maturin/venv selection | `.codex/skills/nteract-python-bindings/` |
| Automerge ownership, output manifests, notebook sync protocol | `.codex/skills/nteract-notebook-sync/` |
| Choosing and running Rust, Python, JS, WASM, E2E, or daemon tests | `.codex/skills/nteract-testing/` |

Use `packages/sift/CLAUDE.md` before touching `packages/sift/`.

## Repo Map

| Path | What lives there |
|------|------------------|
| `apps/notebook/` | Tauri notebook app, React frontend, isolated output iframe |
| `apps/renderer-test/` | Renderer test harness |
| `apps/mcp-app/` | MCP app packaging |
| `crates/notebook/` | Tauri shell and app-side Rust commands |
| `crates/runtimed/` | Central daemon: rooms, runtime agents, env pools, sync server |
| `crates/runt/` | CLI for daemon, kernels, notebooks, and MCP |
| `crates/runt-mcp/` | Rust-native MCP notebook tools |
| `crates/mcp-supervisor/` | `nteract-dev` supervisor/proxy with dev tools |
| `crates/nteract-mcp/`, `crates/runt-mcp-proxy/` | Resilient shipped MCP proxies |
| `crates/notebook-doc/` | Automerge notebook schema, cells, outputs, MIME classification |
| `crates/runtime-doc/` | Runtime state Automerge schema |
| `crates/notebook-protocol/` | Wire request, response, and broadcast types |
| `crates/notebook-sync/` | Sync client and Python-facing document access |
| `crates/runtimed-client/` | Shared client helpers for daemon paths, blobs, and outputs |
| `crates/runtimed-py/` | PyO3 Rust bindings for `python/runtimed` |
| `crates/runtimed-wasm/` | WASM bindings for notebook document operations |
| `crates/kernel-env/` | UV/Conda/Pixi env creation, hashing, pooling, cleanup |
| `crates/kernel-launch/` | Kernel launch/tool bootstrap logic |
| `crates/runt-workspace/` | Per-worktree daemon/cache namespace logic |
| `crates/runt-trust/` | Notebook trust |
| `crates/repr-llm/` | LLM-friendly output summaries |
| `crates/nteract-predicate/`, `crates/sift-wasm/` | Sift compute and WASM bindings |
| `crates/automunge/` | Automerge helper tooling |
| `crates/xtask/` | Build, lint, test, app, daemon, MCP orchestration |
| `packages/notebook-host/` | JS host API for notebook embedding/Tauri transport |
| `packages/runtimed/`, `packages/runtimed-node/` | JS runtimed clients/bindings |
| `packages/sift/` | Table/data viewer package |
| `python/runtimed/` | Python package backed by `crates/runtimed-py` |
| `python/nteract/` | Python MCP wrapper that finds and launches `runt mcp` |
| `python/gremlin/` | Notebook stress-test agent |
| `python/dx/` | Python display helpers |
| `python/prewarm/` | Package prewarm helper |
| `python/nteract-kernel-launcher/` | Python launcher package |
| `docs/` | User-facing feature docs |
| `contributing/` | Maintainer/developer subsystem docs |
| `scripts/` | Install, build, CI, metrics, and smoke-test helpers |

## Command Map

All repo build/test/dev entry points should go through `cargo xtask` unless a
subsystem doc says otherwise.

| Need | Command |
|------|---------|
| Discover current commands | `cargo xtask help` |
| Full debug build | `cargo xtask build` |
| Rust-only rebuild | `cargo xtask build --rust-only` |
| Run bundled debug binary | `cargo xtask run [notebook.ipynb]` |
| Hot-reload notebook dev server | `cargo xtask notebook [notebook.ipynb]` |
| Attach Tauri to existing Vite | `cargo xtask notebook --attach [notebook]` |
| Vite only | `cargo xtask vite` |
| Per-worktree daemon | `cargo xtask dev-daemon` |
| Repo-local MCP supervisor | `cargo xtask run-mcp` |
| Print MCP config | `cargo xtask run-mcp --print-config` |
| MCP inspector UI | `cargo xtask mcp-inspector` |
| Format and fix lint | `cargo xtask lint --fix` |
| CI-style lint check | `cargo xtask lint` |
| Clippy | `cargo xtask clippy` |
| Python integration tests | `cargo xtask integration [filter]` |
| E2E tests | `cargo xtask e2e [build\|test\|test-fixture\|test-all]` |
| Rebuild WASM and renderer plugins | `cargo xtask wasm` |
| Rebuild one WASM target | `cargo xtask wasm runtimed` or `cargo xtask wasm sift` |
| Rebuild renderer plugins only | `cargo xtask renderer-plugins` |
| Verify renderer plugin/wasm drift | `cargo xtask verify-plugins` |
| Build app bundle | `cargo xtask build-app` |
| Build DMG | `cargo xtask build-dmg` |
| Generate icons | `cargo xtask icons [source.png]` |
| Package MCP extension | `cargo xtask mcpb` |
| Regenerate MCP tool cache | `cargo xtask sync-tool-cache` |
| Check dependency budgets | `cargo xtask check-dep-budget` |
| Bump all versioned artifacts | `cargo xtask bump [patch\|minor\|major]` |

Do not launch the notebook GUI from an agent terminal unless the user explicitly
asks; it blocks until the human quits the app.

Run `cargo xtask lint --fix` before committing. Commit and PR titles must use
Conventional Commits:

```text
<type>(<optional-scope>)!: <short imperative summary>
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`,
`perf`, `revert`.

## Daemon And MCP

Development uses per-worktree daemon isolation. Prefer `nteract-dev` tools when
the client exposes them:

| If available | Use for |
|--------------|---------|
| `up` | Idempotently start/repair daemon, MCP child, and optionally Vite |
| `down` | Stop managed Vite; pass `daemon=true` only when you mean it |
| `status` | Inspect supervisor, child, daemon, and process state |
| `logs` | Read daemon logs |
| `vite_logs` | Read Vite logs |

If those tools are not available, stop chasing MCP attachment and use manual
`cargo xtask` commands. For raw `./target/debug/runt ...` commands, pin the
worktree daemon explicitly:

```bash
RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" ./target/debug/runt daemon status
```

Avoid system-wide process killers such as `pkill` and `killall`; they can affect
other worktrees and agents. Use `down`, `./target/debug/runt daemon stop`, or
the relevant `cargo xtask` flow.

Installed stable/nightly MCP servers must be launched with `env -i HOME=$HOME`
so worktree dev environment variables do not leak into system daemons.

Source builds default to the nightly channel. Use `RUNT_BUILD_CHANNEL=stable`
only when explicitly validating stable branding, stable socket/cache paths, or
stable app-launch behavior.

## Python Workspace

The UV workspace root is the repository root. The repo has two important venvs:

| Venv | Purpose |
|------|---------|
| `.venv` | Workspace venv used by `uv run nteract`, MCP, and gremlin |
| `python/runtimed/.venv` | Test venv for `python/runtimed` integration tests |

When rebuilding bindings, always set `VIRTUAL_ENV` so `maturin develop` installs
into the intended venv. See `.codex/skills/nteract-python-bindings/` and
`contributing/runtimed.md`.

## Notebook And Dependency Tools

Use MCP notebook tools (`create_notebook`, `add_dependency`,
`sync_environment`, `execute_cell`, `get_cell`, `run_all_cells`, etc.) when they
are available. Do not hand-write dependency metadata into notebooks unless you
are making test fixtures. Fixture dependency metadata lives at:

```json
{
  "metadata": {
    "runt": {
      "uv": {
        "dependencies": ["pandas>=2.0", "numpy"]
      }
    }
  }
}
```

## Subsystem Docs

| Need | Doc |
|------|-----|
| Architecture overview | `contributing/architecture.md` |
| Development setup | `contributing/development.md` |
| Running tests | `contributing/testing.md` |
| E2E/WebdriverIO | `contributing/e2e.md` |
| Frontend architecture | `contributing/frontend-architecture.md` |
| UI components | `contributing/ui.md` |
| nteract Elements | `contributing/nteract-elements.md` |
| Wire protocol and sync | `contributing/protocol.md` |
| CRDT mutation rules | `contributing/crdt-mutation-guide.md` |
| Widgets | `contributing/widget-development.md` |
| Daemon and Python bindings | `contributing/runtimed.md` |
| Environments | `contributing/environments.md` |
| Output iframe and renderer plugins | `contributing/iframe-isolation.md` |
| TypeScript bindings | `contributing/typescript-bindings.md` |
| Logging | `contributing/logging.md` |
| Build dependencies | `contributing/build-dependencies.md` |
| Releasing | `contributing/releasing.md` |
| Branch/worktree cleanup | `contributing/branch-hygiene.md` |
| User-facing docs | `docs/` |

## High-Risk Invariants

Keep these rules in mind before changing daemon, sync, output, iframe, or env
code. The linked subsystem docs contain the full rationale.

### Async Locks

Never hold a `tokio::sync::Mutex` or `tokio::sync::RwLock` guard across an
`.await`. Use block scoping so the guard is dropped before async work. Prefer
owned actor-loop state or synchronous `std::sync::Mutex` when possible. CI
enforces this with `cargo test -p runtimed --test tokio_mutex_lint`.

### CRDT Writes

Any code path that reads from a CRDT doc, does async work, then writes back must
use `fork()` before the async work and `merge()` afterward. For synchronous
mutation blocks, use `fork_and_merge`. Do not use `fork_at(historical_heads)`;
it can trigger Automerge `MissingOps` panics on interleaved text edits.

Never call `put_object()` on a shared key that another peer can create. Maps and
lists at well-known keys are created by one owner, normally the daemon. Other
peers receive structure through sync.

Never write to the CRDT in response to a daemon broadcast. The daemon already
wrote the change; writing again creates redundant sync traffic and dirty state.

### Ownership Boundaries

| State | Writer |
|-------|--------|
| Cell source, position, type, metadata | Frontend WASM |
| Notebook metadata such as deps/runtime picker | Frontend WASM |
| Cell outputs and execution count | Runtime agent subprocess |
| Execution queue transitions | Coordinator and runtime agent |
| RuntimeStateDoc | Runtime agent and coordinator |

`notebook-doc` owns the document schema and MIME classification.
`notebook-protocol` owns wire types. `notebook-sync` owns sync handles and
Python-facing document access.

### MIME Classification

`notebook-doc::mime` is the single Rust source of truth for MIME classification:
`is_binary_mime()`, `mime_kind()`, and `MimeKind`. WASM resolves `ContentRef`s
to `Inline`, `Url`, or `Blob`; TypeScript should not reimplement binary MIME
classification.

### Protocol Compatibility

Notebook protocol compatibility is not a blanket guarantee that an old UI can
continue normal notebook traffic against a new daemon. The supported upgrade
path closes notebook windows, clears sync handles, swaps the daemon, waits for
readiness/version, and relaunches the new app. Keep upgrade-window Tauri
commands working across that boundary, and prefer typed sync state over stale
request/response variants.

### Iframe Security And Renderers

Never add `allow-same-origin` to the output iframe sandbox. This would let
untrusted notebook output access Tauri APIs.

Heavy renderers such as markdown, Plotly, Vega, and Leaflet load as on-demand
CJS plugins identified by MIME type. Keep the MIME-to-plugin mapping centralized
in the isolated renderer code; see `contributing/iframe-isolation.md`.

The cell list in `NotebookView.tsx` must render in stable DOM order and use CSS
`order` for visual order. Moving iframe DOM nodes reloads outputs and loses
widget state.

### Environment Caches

Use the unified env hash rule from `kernel_env::{uv,conda}`:
`compute_unified_env_hash(deps, env_id)`. The hash always includes `env_id`, so
notebooks do not silently share mutable env directories.

Base package sets are part of capture behavior:
`kernel_env::uv::UV_BASE_PACKAGES` and
`kernel_env::conda::CONDA_BASE_PACKAGES`. Changing them affects newly captured
metadata; existing notebooks keep what they already captured.

Env preservation on eviction requires a saved `.ipynb` path and a runtime env
path that already matches the captured unified-hash directory. Untitled
notebooks and pool envs should not be preserved unless the autosave/claim flow
first gives them a real file path.
