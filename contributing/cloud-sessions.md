# Claude Code on the Web: Cloud Session Bootstrap

This repo wires two scripts so cloud Claude Code sessions (claude.ai/code) can read everything correctly and build the workspace. Local sessions are unaffected. Both scripts gate on `CLAUDE_CODE_REMOTE=true`, which Anthropic sets only in cloud VMs.

## What works in cloud sessions

- **Read everything correctly.** All source is regular git content; no separate fetch step.
- **Build the workspace.** `cargo check`, `cargo build`, `cargo xtask build --rust-only`, `cargo xtask wasm` all succeed once the per-session bootstrap has produced the wasm + renderer-plugin artifacts.

## Out of scope

- Running the daemon (`cargo xtask dev-daemon`). Needs deno for kernel bootstrap.
- Python integration tests (`cargo xtask integration`). Needs maturin, venvs.
- The desktop app (`cargo xtask notebook`). Needs Tauri system deps and a display.

If a workflow above is needed, extend `scripts/cloud-setup.sh` and `scripts/cloud-bootstrap.sh`.

## How it works

Two layers, picked from Anthropic's [cloud sessions documentation](https://code.claude.com/docs/en/cloud):

| | Setup script | SessionStart hook |
| --- | --- | --- |
| Where it's configured | Cloud-env UI at claude.ai/code | `.claude/settings.json` (in repo) |
| When it runs | Once per env snapshot (~7 days) | Every session start and resume |
| Repo present when it runs | **No** (runs before clone) | Yes (runs after clone) |
| Caches as a snapshot | Yes | No |
| Runs as | root | Same user as Claude |
| Canonical content | `scripts/cloud-setup.sh` | `scripts/cloud-bootstrap.sh` |

`scripts/cloud-setup.sh` installs the tools the Anthropic image is missing (`clang`, `wasm-pack`, `corepack`-managed pnpm). `clang` is required for cross-compiling `zstd-sys` (used by `sift-wasm`) to `wasm32-unknown-unknown`. The output snapshots into the cached environment, so subsequent cold sessions skip the install cost. It cannot reference any repo files because the repo isn't cloned yet when it runs.

`scripts/cloud-bootstrap.sh` runs every session, after the clone. It does `pnpm install --frozen-lockfile`, builds the gitignored wasm + renderer-plugin artifacts when missing (see below), and warms the cargo registry. All steps are idempotent: fast no-ops when caches are warm.

## Why we build wasm artifacts at session start

The repo's `runtimed` crate `include_bytes!`-embeds renderer plugin bundles and `sift_wasm.wasm`. Those outputs live under four gitignored directories produced by `cargo xtask wasm`:

- `apps/notebook/src/wasm/runtimed-wasm/`
- `crates/sift-wasm/pkg/`
- `apps/notebook/src/renderer-plugins/`
- `crates/runt-mcp/assets/plugins/`

A fresh cloud clone has none of them, and `runtimed`'s `build.rs` panics with "Missing renderer plugin asset" on the first `cargo build`. The bootstrap detects this (one canonical file probe per output dir) and runs `cargo xtask wasm` once.

The bootstrap retries `cargo xtask wasm` up to three times with 4s and 8s backoff. The dominant flake is rustup re-fetching the toolchain channel manifest from `static.rust-lang.org` and getting a 5xx; backoff rides out the blip.

## Configuration

### One-time setup (per cloud environment)

Open `scripts/cloud-setup.sh` and copy everything below the comment block. In the cloud-env UI at claude.ai/code, paste that body verbatim into the **Setup script** field.

**Do not** put `bash scripts/cloud-setup.sh` in the field. Anthropic runs the setup script before cloning the repo, so any path-based reference fails with `No such file or directory`. The repo file is the canonical source; keep the UI field in sync when the script changes.

If the field is unset or empty, `cargo xtask wasm` in the per-session bootstrap will skip (wasm-pack missing) and the agent's first `cargo build` will fail in `runtimed`'s build.rs. The bootstrap log explains what to do.

### Per-session bootstrap (already wired)

`.claude/settings.json` has a `SessionStart` hook that runs `scripts/cloud-bootstrap.sh` on every cloud session. No setup needed beyond pasting the setup-script body above.

## Debugging

The bootstrap writes a full transcript to `/tmp/cloud-bootstrap.log` on every session. If something looks off, ask Claude to `Read` that file.

The bootstrap always exits 0; a failure never blocks the session. The agent can read the log and recover (e.g., manually run `pnpm install` or `cargo xtask wasm`).

Common symptoms:

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `cargo build` fails with `Missing renderer plugin asset` | wasm build didn't run, or all 3 retries failed | Check `/tmp/cloud-bootstrap.log`. If `wasm-pack` is missing, configure the setup script (above). If retries hit a non-transient error, run `cargo xtask wasm` manually. |
| `cargo xtask wasm` fails with `ToolNotFound: failed to find tool "clang"` | `clang` not installed; needed by `zstd-sys` for sift-wasm cross-compilation | `sudo apt-get install -y clang`, then re-run `cargo xtask wasm`. Update the cloud-env setup script if it's missing `clang`. |
| Bootstrap reports `pnpm install: FAILED` | Lockfile drift or proxy issue | Check the log; try `pnpm install --force`. |

## Future work

- Pre-baking wasm + plugin outputs into the snapshot (publish them as a GitHub release asset and pull from `release-assets.githubusercontent.com`, which is on the default Trusted allowlist) would eliminate per-session `cargo xtask wasm` cost. Worth considering once cloud usage justifies the maintenance burden of cutting an asset release on every WASM-affecting PR.
- Adding deno + uv venv setup unlocks `cargo xtask integration` (tier iii) for cloud test runs. Skip until a concrete cloud workflow needs it.
