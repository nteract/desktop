#!/bin/bash
# Cloud-environment one-time setup for Claude Code on the web.
#
# IMPORTANT: paste the body of this script (everything below the comment
# block) verbatim into the cloud-environment "Setup script" field at
# claude.ai/code. Do NOT point the field at this file path with
# `bash scripts/cloud-setup.sh`. Anthropic runs the setup script BEFORE
# cloning the repo, so any path-based reference fails with
# "No such file or directory". This file lives in the repo as the canonical
# reference; keep the UI field in sync when this changes.
#
# Runs as root in a fresh VM with no repo present. The Anthropic image
# already ships rust, cargo, node 20+, pnpm via corepack, uv, and ripgrep,
# so this script only fills the gaps the project needs for tier-(i)+(ii)
# work (read everything correctly, build the workspace).
#
# Output is filesystem-cached as a snapshot reused across cloud sessions
# for roughly seven days, so heavy installs (apt, cargo install) only pay
# their cost when the snapshot is rebuilt.
#
# Repo-aware warming (pnpm install, cargo fetch, build wasm + plugin assets)
# lives in scripts/cloud-bootstrap.sh, which runs after the per-session
# repo clone.
#
# Out of scope: deno, pixi, maturin, Tauri system deps. Add those when a
# concrete cloud workflow needs them.

set -euo pipefail

[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0

corepack enable

# clang is required for cross-compiling zstd-sys (used by sift-wasm) to
# wasm32-unknown-unknown. Without it, `cargo xtask wasm` fails on the
# sift-wasm step. The Anthropic base image ships gcc but not clang.
if ! command -v clang >/dev/null 2>&1; then
  apt-get update -qq && apt-get install -y -qq clang >/dev/null
fi

if ! command -v wasm-pack >/dev/null 2>&1; then
  cargo install --locked wasm-pack
fi
