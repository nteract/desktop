# Agent Instructions

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This is a map. Subsystem details live in `contributing/`, auto-loaded rules live in `.claude/rules/`, and operational recipes live in `.claude/skills/` and `.codex/skills/`. Run `cargo xtask help` for build commands.

Claude-specific skills live in `.claude/skills/`. Use when the task matches:
- `automerge-sync` for sync protocol internals, reconnection, peer state lifecycle, in-flight suppression, catch_unwind recovery, and convergence debugging
- `mcp-session-lifecycle` for MCP proxy supervision, daemon watch loop, session state, rejoin/reconnect races, and room eviction
- `sync-protocol-patterns` for higher-level protocol design: comparing automerge-repo/samod/nteract architectures, adding new sync streams, heads tracking patterns, and connection lifecycle decisions
- `automerge-document-model` for Automerge internals: OpSet, ChangeGraph, actor tables, save/load lifecycle, fork/merge semantics, #1187 panic root cause, and document size reasoning
- `execution-pipeline` for end-to-end cell execution: required_heads → ExecuteCell → CellQueued → RuntimeStateDoc polling → output-sync grace → output resolution. Use when debugging missing outputs, execution timeouts, or stale results

Codex-specific repo skills live in `.codex/skills/`. Prefer them when the task matches:
- `nteract-daemon-dev` for per-worktree daemon lifecycle, socket setup, and daemon-backed verification
- `nteract-python-bindings` for `maturin develop`, venv selection, and MCP server work
- `nteract-automerge-protocol` for Automerge semantics, sync state, typed frame protocol, storage boundaries, and samod/subduction-informed protocol design
- `nteract-notebook-sync` for Automerge ownership, output manifests, and sync-path changes
- `nteract-testing` for choosing and running the right verification path

## Subsystem guides

| Topic | Doc |
|------|-----|
| Architecture overview | `contributing/architecture.md` |
| Development setup (direnv, lld, sccache, build cache) | `contributing/development.md` |
| Daemon, Python bindings, MCP, telemetry, channels | `contributing/runtimed.md` |
| Tests | `contributing/testing.md` |
| E2E (WebdriverIO) | `contributing/e2e.md` |
| Frontend / UI / nteract Elements | `contributing/frontend-architecture.md`, `contributing/ui.md`, `contributing/nteract-elements.md` |
| Wire protocol & sync | `contributing/protocol.md` |
| Widgets | `contributing/widget-development.md` |
| Environments / trust | `contributing/environments.md` |
| Iframe sandbox & renderer plugins | `contributing/iframe-isolation.md` |
| CRDT mutation rules | `contributing/crdt-mutation-guide.md` |
| TypeScript bindings (ts-rs) | `contributing/typescript-bindings.md` |
| Logging | `contributing/logging.md` |
| Build deps / releasing / branch hygiene | `contributing/build-dependencies.md`, `contributing/releasing.md`, `contributing/branch-hygiene.md` |

## MCP servers

Three may be visible. Pick by purpose. Full details in `.claude/rules/mcp-servers.md` (auto-loaded everywhere).

- **`nteract-dev`** - default for development. Per-worktree dev daemon, dev tools (`up`, `down`, `status`, `logs`, `vite_logs`) plus 26 proxied notebook tools. Prefer `up` over manual `cargo xtask dev-daemon`.
- **`nteract-nightly`** - system nightly daemon. Diagnostics only. Never used for source changes.
- **`nteract`** - system stable daemon. Diagnostics only.

If `nteract-dev` is unavailable, fall back to `cargo xtask` (it derives the worktree env on its own; direnv not required). Never fall back to system MCP servers for dev work.

## Workspace crates

| Crate | Purpose |
|-------|---------|
| `runtimed` | Daemon - env pools, notebook sync, runtime agent coordination |
| `runtimed-client` | Shared client lib - output resolution, daemon paths, pool client, telemetry |
| `runtimed-py` | Python bindings (PyO3/maturin) |
| `runtimed-wasm` | WASM bindings for the notebook doc |
| `notebook` | Tauri desktop app |
| `notebook-wire` | Frame bytes, preamble constants, frame caps, typed-frame enum, session-control status shapes |
| `notebook-doc` | Automerge schema, cell CRUD, nbformat fallback fields, MIME classification, `CellChangeset` |
| `notebook-protocol` | Wire types (requests, responses, broadcasts) |
| `notebook-sync` | `DocHandle`, sync infrastructure, per-cell Python accessors |
| `runt` | CLI - daemon mgmt, kernel control, `runt mcp` |
| `runt-mcp` | Rust-native MCP server (26 tools) |
| `runt-mcp-proxy` | Resilient proxy for `runt mcp` - supervision, restart, session tracking |
| `runt-trust` | Notebook trust (HMAC-SHA256 over deps) |
| `runt-workspace` | Per-worktree daemon isolation |
| `kernel-launch` | Kernel launching, tool bootstrapping |
| `kernel-env` | UV + Conda env management |
| `runtime-doc` | Daemon-authoritative runtime state CRDT schema and handle |
| `repr-llm` | LLM-friendly text summaries (synthesizes `text/llm+plain`) |
| `nteract-predicate` | Pure-Rust dataframe/Arrow compute kernels (backs Sift) |
| `sift-wasm` | WASM bindings for `nteract-predicate` |
| `mcp-supervisor` | `nteract-dev` server - proxies `runt mcp` + dev tools |
| `nteract-mcp` | Resilient MCP proxy shipped as sidecar / `.mcpb` |
| `xtask` | Build orchestration |

## Required before commit

```bash
cargo xtask lint --fix
```

No pre-commit hook. CI rejects unformatted PRs. `cargo xtask clippy` runs lints. `cargo xtask help` is the source of truth for everything else.

## Commit and PR title format

Conventional Commits, required for both commits and PR titles:

```
<type>(<optional-scope>)!: <short imperative summary>
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`.

## Workspace description

In a worktree, set a human-readable label (`.context/` is gitignored):

```bash
mkdir -p .context && echo "Your description" > .context/workspace-description
```

## Don't launch the desktop app from an agent terminal

`cargo xtask notebook` opens a GUI that blocks until ⌘Q. The agent will misinterpret the exit. Let the human launch it from their own terminal.

## Load-bearing invariants

Most invariants live in `.claude/rules/*.md` and auto-load when you edit matching paths. Read them before changing related code. Two that don't fit any rule scope are stated in full below.

| Invariant | Where |
|-----------|-------|
| Daemon as source of truth, RuntimeStateDoc, `is_binary_mime` contract, crate boundaries, state ownership, blob store | `.claude/rules/architecture.md` |
| Fork+merge for async CRDT mutations, no independent `put_object` on shared keys | `.claude/rules/crdt-mutations.md` |
| Iframe sandbox (`allow-same-origin` is forbidden), renderer plugins | `.claude/rules/iframe-isolation.md` |
| Wire protocol versions, upgrade compatibility | `.claude/rules/protocol.md` |
| Two-stage env detection, trust | `.claude/rules/environments.md` |
| Frontend layout / conventions | `.claude/rules/frontend-architecture.md` |
| Widget architecture (parent/iframe split) | `.claude/rules/widget-development.md` |
| UI component stack (Shadcn + nteract) | `.claude/rules/ui-components.md` |
| Logging | `.claude/rules/logging.md` |
| Unified env hash, preserve-on-eviction, hot-sync coherence | `contributing/environments.md` |

### No tokio mutex guards held across `.await`

Never hold a `tokio::sync::Mutex` or `RwLock` guard across an `.await`. Convoy deadlocks if the holder suspends. CI enforces via `cargo test -p runtimed --test tokio_mutex_lint` (no exceptions). Use block scoping (not `drop()`) so the lint can verify. Prefer owned state in `select!` loops over `Arc<Mutex<...>>`. Use `std::sync::Mutex` for sync-only access.

### Cell list stable DOM order

`apps/notebook/src/components/NotebookView.tsx` MUST render cells in stable DOM order (sorted by cell ID) and use CSS `order` for visual positioning. Iterating `cellIds` directly causes React to call `insertBefore` on reorder, which destroys and reloads any `<iframe>` inside - visible as white flashes, lost widget state, re-rendered outputs. Iterate `stableDomOrder`; the parent is `display: flex; flex-direction: column` and each child sets `order`. Fallback iframe-reload detection lives in `src/components/isolated/isolated-frame.tsx`.

## Notebook files

Don't write `.ipynb` files by hand with dependency metadata. Use the MCP tools (`create_notebook`, `add_dependency`) - the metadata schema is internal. Test fixtures that need deps put them at `metadata.runt.uv.dependencies`.
