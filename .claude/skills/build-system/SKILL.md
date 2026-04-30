---
name: build-system
description: Build, rebuild, and manage build artifacts. Use when building the app, WASM, or managing build dependencies.
---

# Build System

For commands and dev workflows, see `CLAUDE.md` → "Build System" and run `cargo xtask help`.

## How `cargo xtask build` works

Four phases:

0. **Build artifact check** — verify the gitignored wasm + renderer-plugin outputs exist. If any of the four output dirs is empty, run `cargo xtask wasm` first so `runtimed`'s `include_bytes!` and the frontend's virtual modules can resolve.

1. **Single Rust compilation** — `cargo build -p runtimed -p runt -p mcp-supervisor -p notebook` in one invocation (workspace feature unification happens once, so the final tauri step doesn't recompile). Sidecars (`runtimed`, `runt`, `nteract-mcp`) are copied to `crates/notebook/binaries/` for Tauri bundling.

2. **Frontend build** — `pnpm build` (TypeScript + Vite). Must finish before phase 3.

3. **Tauri link** — `cargo tauri build --debug --no-bundle` links the notebook binary with embedded frontend assets. Rust is cached from phase 1.

`--rust-only` skips the frontend build in phase 2 and reuses existing `apps/notebook/dist/`.

Python bindings (`maturin develop`) are no longer part of `cargo xtask build`. Run `cargo xtask integration` (builds bindings for pytest), use the nteract-dev MCP `up rebuild=true` tool, or invoke `maturin develop` directly under `crates/runtimed-py/` when you need the `.so`. CI builds them explicitly.

## Key constraints

- All Rust targets must build in **one** `cargo build` call to avoid feature-unification recompilation.
- `runtimed` + `runt` must exist in `crates/notebook/binaries/` before the tauri step (`bundle.externalBin`).
- WASM and renderer-plugin outputs are gitignored. `cargo xtask build` auto-runs `cargo xtask wasm` when any of the four output dirs is empty; `runtimed`'s `build.rs` panics with a "run `cargo xtask wasm`" message if you skip xtask and call `cargo build` directly.

## Crate dependency graph

Leaf crates (no internal deps): `kernel-launch`, `notebook-wire`, `runt-trust`, `runt-workspace`

Shared:
- `notebook-doc` → no internal deps
- `notebook-protocol` → `notebook-wire`, `kernel-env`
- `notebook-sync` → `notebook-doc`, `notebook-protocol`
- `runtimed` → `notebook-doc`, `notebook-protocol`, `notebook-wire`, `kernel-launch`, `kernel-env`, `runt-trust`, `runt-workspace`

App binaries:
- `notebook` (Tauri) → `runtimed-client`, `notebook-doc`, `notebook-protocol`, `notebook-sync`, `notebook-wire`, `runt-trust`, `runt-workspace`
- `runt` → `runtimed-client`, `notebook-doc`, `runt-workspace`, `kernel-env`, `runt-mcp`
- `runtimed` → `runtimed-client`, `notebook-doc`, `notebook-protocol`, `notebook-wire`, `kernel-launch`, `kernel-env`, `runt-trust`, `runt-workspace`
- `runtimed-py` → `runtimed-client`, `notebook-doc`, `notebook-protocol`, `kernel-env`, `notebook-sync`, `runt-workspace`
- `runtimed-wasm` → `notebook-doc`, `notebook-wire`

## WASM rebuild

The wasm + renderer-plugin outputs are gitignored. Run after changing `crates/runtimed-wasm/`, `crates/sift-wasm/`, `crates/notebook-doc/`, `crates/notebook-wire/`, or `scripts/build-renderer-plugins.ts`:

```bash
cargo xtask wasm             # rebuild runtimed-wasm + sift-wasm + chained renderer plugins
cargo xtask wasm runtimed    # only runtimed-wasm
cargo xtask wasm sift        # only sift-wasm (chains plugins)
```

`cargo xtask build` calls this automatically when any of the four output directories is missing on disk, so a fresh clone can go straight to `cargo xtask build`.
