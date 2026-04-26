#!/bin/bash
# Cloud-environment one-time setup for Claude Code on the web.
#
# Configure this in the cloud-environment "Setup script" field at
# claude.ai/code as the literal one-liner:
#
#     bash scripts/cloud-setup.sh
#
# Runs as root in a fresh VM with the repo cloned at CWD. The Anthropic image
# already ships rust, cargo, node 20+, pnpm via corepack, uv, and ripgrep, so
# this script only fills the gaps the project needs for tier-(i)+(ii) work
# (read everything correctly, build the workspace).
#
# Output is filesystem-cached as a snapshot reused across cloud sessions for
# roughly seven days, so heavy installs (apt, registry warmup) only pay their
# cost when the snapshot is rebuilt.
#
# LFS NOTE: the Anthropic sandbox blocks the GitHub LFS object hosts
# (media.githubusercontent.com, github-cloud.githubusercontent.com) and the
# local git proxy returns 502 on the LFS batch endpoint, so `git lfs pull`
# cannot work from cloud sessions. The per-session bootstrap regenerates
# LFS-tracked build artifacts from source via `cargo xtask wasm` instead.
#
# Out of scope: deno, pixi, maturin, Tauri system deps. Add those when a
# concrete cloud workflow needs them.

set -euo pipefail

log() { printf '[cloud-setup] %s\n' "$*"; }

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  log "Not a cloud session (CLAUDE_CODE_REMOTE != true). Exiting."
  exit 0
fi

log "Installing git-lfs (not preinstalled in the Anthropic cloud image)."
apt-get update -qq
apt-get install -y -qq git-lfs
git lfs install --system --skip-smudge --skip-repo

log "Enabling pnpm via corepack."
corepack enable

log "Installing wasm-pack (needed by 'cargo xtask wasm' to regenerate LFS artifacts)."
if ! command -v wasm-pack >/dev/null 2>&1; then
  cargo install --locked wasm-pack
fi

if [ -f pnpm-lock.yaml ]; then
  log "Warming pnpm store from lockfile."
  pnpm fetch || log "pnpm fetch failed; continuing (per-session install will retry)."
fi

if [ -f Cargo.toml ]; then
  log "Warming cargo registry."
  cargo fetch || log "cargo fetch failed; continuing (per-session fetch will retry)."
fi

log "Setup complete."
