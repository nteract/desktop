#!/usr/bin/env bash
# Wrapper for launching the nteract MCP server in dev mode.
#
# MCP clients often spawn servers with a minimal PATH and may not
# resolve env var references (e.g. $ZED_WORKTREE_ROOT). This script
# augments PATH for common tool locations and derives the workspace
# path from the project root.
#
# Example MCP client config:
#   {
#     "command": "bash",
#     "args": ["scripts/dev-nteract-mcp.sh"],
#     "env": { "RUNTIMED_DEV": "1" }
#   }

set -euo pipefail

# MCP clients may spawn servers with a minimal PATH that doesn't include
# user-local tool directories. Add the common locations for uv when HOME is set.
if [ -n "${HOME:-}" ]; then
    export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
else
    export PATH="/opt/homebrew/bin:$PATH"
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Log to a file so we can debug MCP client launch failures
LOGDIR="$PROJECT_ROOT/.context"
mkdir -p "$LOGDIR"
LOGFILE="$LOGDIR/nteract-mcp.log"

log() { echo "$(date '+%H:%M:%S') [dev-nteract-mcp] $*" >> "$LOGFILE"; }

log "starting (pid=$$, ppid=$PPID)"
log "PWD=$PWD"
log "PROJECT_ROOT=$PROJECT_ROOT"
log "RUNTIMED_DEV=${RUNTIMED_DEV:-<unset>}"
log "RUNTIMED_WORKSPACE_PATH=${RUNTIMED_WORKSPACE_PATH:-<unset>}"

# Write pidfile for lifecycle tracking
PIDFILE="$LOGDIR/nteract-mcp.pid"
echo $$ > "$PIDFILE"
trap 'rm -f "$PIDFILE"; log "exiting (pid=$$)"' EXIT

# Derive workspace path from the project root
export RUNTIMED_DEV="${RUNTIMED_DEV:-1}"
export RUNTIMED_WORKSPACE_PATH="${RUNTIMED_WORKSPACE_PATH:-$PROJECT_ROOT}"

log "RUNTIMED_WORKSPACE_PATH=$RUNTIMED_WORKSPACE_PATH"
log "launching uv run --no-sync --directory $PROJECT_ROOT/python nteract"

# Keep bash alive so the pidfile and trap stay valid.
# Route stderr to the log file so MCP stdio transport stays clean.
# Disable set -e around the command so we can capture and log the exit code.
set +e
uv run --no-sync --directory "$PROJECT_ROOT/python" nteract 2>> "$LOGFILE"
EXIT_CODE=$?
set -e
log "nteract exited with code $EXIT_CODE"
exit $EXIT_CODE
