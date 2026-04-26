#!/bin/bash
# Per-session SessionStart hook for Claude Code on the web.
#
# Wired in via .claude/settings.json. Runs every time Claude starts in a
# cloud session. Idempotent and fast on warm caches; first run after a snapshot
# rebuild does the full work.
#
# Scope: prepare the workspace for tier-(i) read and tier-(ii) build. We do
# NOT pull LFS — the Anthropic sandbox blocks the GitHub LFS object hosts and
# the local git proxy returns 502 on the batch endpoint. Instead we regenerate
# the LFS-tracked build artifacts from source via `cargo xtask wasm`, which
# produces real .wasm + plugin .js/.css bundles in their checked-in locations.
#
# Always exits 0 so a bootstrap failure never blocks the session — the agent
# can still read the log and recover. Full transcript at /tmp/cloud-bootstrap.log.

set -uo pipefail

LOG=/tmp/cloud-bootstrap.log
: > "$LOG"

log() {
  printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" | tee -a "$LOG"
}

run() {
  printf '\n>>> %s\n' "$*" >> "$LOG"
  "$@" >> "$LOG" 2>&1
}

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(pwd)}" 2>/dev/null || exit 0

log "cloud bootstrap starting (full log: $LOG)"

# pnpm install. Frozen lockfile is fast when the snapshot already populated
# node_modules; otherwise this is the first-session cost.
if [ -f pnpm-lock.yaml ] && command -v pnpm >/dev/null 2>&1; then
  if run pnpm install --frozen-lockfile --prefer-offline; then
    log "  pnpm install: ok"
  else
    log "  pnpm install: FAILED (see $LOG)"
  fi
else
  log "  pnpm install: skipped (no pnpm-lock.yaml or pnpm missing)"
fi

# Detect LFS pointer files in the checked-in artifact locations. A pointer is
# a small text file starting with `version https://git-lfs.github.com/spec/v1`.
# If any are present, regenerate via `cargo xtask wasm` so include_bytes!,
# frontend tests, and Reads of plugin source all see real content.
LFS_PROBE=crates/sift-wasm/pkg/sift_wasm_bg.wasm
NEEDS_REGEN=0
if [ -f "$LFS_PROBE" ] && head -c 40 "$LFS_PROBE" 2>/dev/null | grep -q 'git-lfs.github.com/spec'; then
  NEEDS_REGEN=1
fi

if [ "$NEEDS_REGEN" = "1" ]; then
  log "  lfs artifacts: pointer files detected, regenerating via cargo xtask wasm"
  if command -v wasm-pack >/dev/null 2>&1; then
    if run cargo xtask wasm; then
      log "  cargo xtask wasm: ok"
    else
      log "  cargo xtask wasm: FAILED (see $LOG) — runtimed/runt won't compile, frontend plugin tests will fail"
    fi
  else
    log "  cargo xtask wasm: skipped (wasm-pack missing; configure cloud-env setup script — see contributing/cloud-sessions.md)"
  fi
else
  log "  lfs artifacts: real content present (skipping wasm regen)"
fi

# Cargo registry warmup. Cheap when the snapshot already cached it.
if [ -f Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
  if run cargo fetch; then
    log "  cargo fetch: ok"
  else
    log "  cargo fetch: FAILED (see $LOG)"
  fi
fi

log "cloud bootstrap done"
exit 0
