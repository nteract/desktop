#!/bin/bash
# Per-session SessionStart hook for Claude Code on the web.
#
# Wired in via .claude/settings.json. Runs every time Claude starts in a
# cloud session. Idempotent and fast on warm caches; first run after a snapshot
# rebuild does the full work.
#
# Scope: prepare the workspace for tier-(i) read and tier-(ii) build. The
# wasm + renderer-plugin artifacts that runtimed embeds via include_bytes!
# are gitignored, so a fresh clone has no copy. Bootstrap builds them via
# `cargo xtask wasm` so subsequent `cargo build` / `cargo check` calls
# don't blow up in runtimed's build.rs.
#
# Always exits 0 so a bootstrap failure never blocks the session. The agent
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

# Detect missing wasm + renderer-plugin artifacts. They're gitignored, so a
# fresh clone has none of them; on warm sessions they're already cached.
# Touch one canonical output from each of the four directories `cargo xtask
# wasm` produces.
ARTIFACT_PROBES=(
  crates/sift-wasm/pkg/sift_wasm_bg.wasm
  apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm
  apps/notebook/src/renderer-plugins/sift.js
  crates/runt-mcp/assets/plugins/sift_wasm.wasm
)
NEEDS_BUILD=0
for probe in "${ARTIFACT_PROBES[@]}"; do
  if [ ! -s "$probe" ]; then
    NEEDS_BUILD=1
    break
  fi
done

if [ "$NEEDS_BUILD" = "1" ]; then
  log "  build artifacts: missing, running cargo xtask wasm"
  if command -v wasm-pack >/dev/null 2>&1; then
    # Retry on transient network failures. The dominant flake is rustup
    # re-fetching the toolchain channel manifest from static.rust-lang.org
    # and getting a 5xx; backoff is enough to ride out the blip.
    WASM_OK=0
    for attempt in 1 2 3; do
      if run cargo xtask wasm; then
        WASM_OK=1
        break
      fi
      if [ "$attempt" -lt 3 ]; then
        delay=$((attempt * 4))
        log "  cargo xtask wasm: attempt $attempt failed, retrying in ${delay}s"
        sleep "$delay"
      fi
    done
    if [ "$WASM_OK" = "1" ]; then
      log "  cargo xtask wasm: ok"
    else
      log "  cargo xtask wasm: FAILED after 3 attempts (see $LOG); runtimed/runt won't compile, frontend plugin tests will fail"
    fi
  else
    log "  cargo xtask wasm: skipped (wasm-pack missing; configure cloud-env setup script. See contributing/cloud-sessions.md)"
  fi
else
  log "  build artifacts: present (skipping wasm build)"
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
