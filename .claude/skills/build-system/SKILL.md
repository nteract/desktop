---
name: build-system
description: Build, rebuild, and manage build artifacts. Use when building the app, WASM, or managing build dependencies.
---

# Build System

For commands and dev workflows, see `CLAUDE.md` → "Build System" and run `cargo xtask help`.

## How `cargo xtask build` works

Three phases:

1. **Single Rust compilation** — `cargo build -p runtimed -p runt-cli -p mcp-supervisor -p notebook` in one invocation (workspace feature unification happens once, so the final tauri step doesn't recompile). Sidecars (`runtimed`, `runt`) are copied to `crates/notebook/binaries/` for Tauri bundling.

2. **Frontend + Python bindings in parallel** — `pnpm build` (TypeScript + Vite) and `maturin develop` (Python `.so`) run concurrently. Both must finish before phase 3.

3. **Tauri link** — `cargo tauri build --debug --no-bundle` links the notebook binary with embedded frontend assets. Rust is cached from phase 1.

`--rust-only` skips the frontend build in phase 2 and reuses existing `apps/notebook/dist/`.

## Key constraints

- All Rust targets must build in **one** `cargo build` call to avoid feature-unification recompilation.
- `runtimed` + `runt` must exist in `crates/notebook/binaries/` before the tauri step (`bundle.externalBin`).
- WASM artifacts are committed and validated (not rebuilt) — `ensure_wasm_resolved()` checks for git-lfs pointers.

## Crate dependency graph

Leaf crates (no internal deps): `tauri-jupyter`, `kernel-launch`, `runt-trust`, `runt-workspace`

Shared:
- `notebook-doc` → no internal deps
- `notebook-protocol` → `notebook-doc`, `kernel-env`
- `notebook-sync` → `notebook-doc`, `notebook-protocol`
- `runtimed` → `notebook-doc`, `notebook-protocol`, `kernel-launch`, `kernel-env`, `runt-trust`, `runt-workspace`

App binaries:
- `notebook` (Tauri) → `runtimed`, `notebook-sync`, `runt-trust`, `runt-workspace`
- `runt-cli` → `runtimed`, `runt-workspace`
- `runtimed-py` → `runtimed`, `notebook-doc`, `notebook-protocol`, `notebook-sync`, `runt-workspace`
- `runtimed-wasm` → `notebook-doc`

## WASM rebuild

Only needed when changing `crates/runtimed-wasm/` or `crates/notebook-doc/`:

```bash
wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm
# Commit the output
```
