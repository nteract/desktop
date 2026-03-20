---
name: build-system
description: Build, rebuild, and manage build artifacts. Use when building the app, WASM, or managing build dependencies.
---

# Build System

## Build Order

When `cargo xtask build` runs, it does these steps in order:

1. `pnpm install` — install frontend dependencies
2. `wasm-pack build crates/runtimed-wasm` — build WASM (only if changed; artifacts are committed)
3. `pnpm --dir apps/notebook build` — build frontend (includes isolated-renderer inline)
4. `cargo build --release -p runtimed -p runt-cli` — build sidecar binaries
5. Copy binaries to `crates/notebook/binaries/` — Tauri expects them there
6. `cargo tauri build` — bundle final app

## Key Constraints

| Constraint | Why |
|---|---|
| Frontend must build before Tauri | `tauri.conf.json` `beforeBuildCommand` runs `pnpm --dir apps/notebook build` |
| `runtimed` + `runt` must exist in `crates/notebook/binaries/` | `tauri.conf.json` lists them in `bundle.externalBin` |
| `runtimed-wasm` must build before frontend | wasm-pack output in `apps/notebook/src/wasm/runtimed-wasm/`; Vite imports at build time |
| WASM artifacts are committed | Developers don't need wasm-pack for normal development |
| `isolated-renderer` built inline | Vite plugin builds it as a virtual module (no separate step) |
| Python wheel uses maturin | `python/runtimed/pyproject.toml` points maturin at `crates/runtimed-py/Cargo.toml` |
| `notebook-doc` is shared | Used by `runtimed`, `runtimed-wasm`, and `runtimed-py` |

## Crate Dependency Overview

Leaf crates (no internal deps): `tauri-jupyter`, `kernel-launch`, `runt-trust`, `runt-workspace`

Shared crates:
- `notebook-doc` depends on nothing internal
- `notebook-protocol` depends on `notebook-doc`, `kernel-env`
- `notebook-sync` depends on `notebook-doc`, `notebook-protocol`
- `runtimed` depends on `notebook-doc`, `notebook-protocol`, `kernel-launch`, `kernel-env`, `runt-trust`, `runt-workspace`

App crates:
- `notebook` (Tauri app) depends on `tauri-jupyter`, `runtimed`, `notebook-sync`, `runt-trust`, `runt-workspace`
- `runt-cli` depends on `runtimed`, `runt-workspace`
- `runtimed-py` depends on `runtimed`, `notebook-doc`, `notebook-protocol`, `notebook-sync`, `runt-workspace`
- `runtimed-wasm` depends on `notebook-doc`

## WASM Rebuild

Only needed when changing `crates/runtimed-wasm/` or `crates/notebook-doc/`:

```bash
wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm
# Commit the output -- WASM artifacts are checked into the repo
```

## Development Workflows

### `cargo xtask dev` — One-shot setup + dev

```bash
cargo xtask dev                          # Full: pnpm install + build + daemon + app
cargo xtask dev --skip-install --skip-build  # Fast repeat launch
```

Runs pnpm install, builds everything, starts dev daemon, waits for it, launches notebook.

### `cargo xtask notebook` — Hot reload

```bash
cargo xtask notebook
```

Uses Vite dev server on port 5174. React component changes hot-reload instantly. Best for UI development.

### `cargo xtask vite` + `notebook --attach` — Multi-window testing

```bash
# Terminal 1: Start Vite standalone (stays running)
cargo xtask vite

# Terminal 2+: Attach Tauri to existing Vite
cargo xtask notebook --attach
```

Closing Tauri windows doesn't kill Vite. Useful for collaboration testing.

### `cargo xtask build` + `run` — Debug build

```bash
cargo xtask build           # Full build (frontend + rust)
cargo xtask run             # Run the bundled binary
cargo xtask run path/to/notebook.ipynb  # With specific notebook
```

Builds debug binary with frontend assets bundled. Emits JS source maps for devtools.

### `cargo xtask build --rust-only` — Fast Rust iteration

```bash
cargo xtask build              # First time: full build
cargo xtask build --rust-only  # Subsequent: skip frontend (much faster)
cargo xtask run
```

Ideal for daemon development. Build frontend once, iterate on Rust.

### `cargo xtask build-app` / `build-dmg` — Release builds

Mostly CI. Use locally only for testing app bundle structure, file associations, icons.

## Build Cache (sccache)

```bash
brew install sccache   # macOS
```

Auto-detected by xtask. Shares compiled artifacts across worktrees. Without it, each worktree rebuilds ~788 crates from scratch.

## Before You Commit

```bash
cargo xtask lint --fix
```

Formats Rust, lints/formats TypeScript/JavaScript with Biome, lints/formats Python with ruff. CI rejects PRs that fail.

For check-only mode: `cargo xtask lint`

## Test Notebooks

Test notebooks: `crates/notebook/fixtures/audit-test/`
Sample notebooks: `crates/notebook/resources/sample-notebooks/`

```bash
cargo xtask build
./target/debug/notebook crates/notebook/fixtures/audit-test/1-vanilla.ipynb
```

## Quick Reference

| Task | Command |
|------|---------|
| One-shot setup | `cargo xtask dev` |
| Hot reload | `cargo xtask notebook` |
| Standalone Vite | `cargo xtask vite` |
| Attach to Vite | `cargo xtask notebook --attach` |
| Full debug build | `cargo xtask build` |
| Rust-only rebuild | `cargo xtask build --rust-only` |
| Run bundled binary | `cargo xtask run` |
| Build release .app | `cargo xtask build-app` |
| Build release DMG | `cargo xtask build-dmg` |
| Lint (check) | `cargo xtask lint` |
| Lint (fix) | `cargo xtask lint --fix` |
| See all commands | `cargo xtask help` |
