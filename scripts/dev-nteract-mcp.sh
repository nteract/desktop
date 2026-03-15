#!/usr/bin/env bash
# Wrapper for launching the nteract MCP server in dev mode.
#
# Zed doesn't resolve $ZED_WORKTREE_ROOT in context_servers env fields,
# but PWD is set to the project root. This script bridges the gap.
#
# Usage (in .zed/settings.json):
#   {
#     "context_servers": {
#       "nteract": {
#         "command": "bash",
#         "args": ["scripts/dev-nteract-mcp.sh"],
#         "env": { "RUNTIMED_DEV": "1" }
#       }
#     }
#   }

set -euo pipefail

# Zed spawns context servers with a minimal PATH that may not include
# user-local tool directories. Add the common locations for uv.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Log to a file so we can debug Zed launch failures
LOGDIR="$PROJECT_ROOT/.context"
mkdir -p "$LOGDIR"
LOGFILE="$LOGDIR/nteract-mcp.log"

log() { echo "$(date '+%H:%M:%S') [dev-nteract-mcp] $*" >> "$LOGFILE"; }

log "starting (pid=$$, ppid=$PPID)"
log "PWD=$PWD"
log "PROJECT_ROOT=$PROJECT_ROOT"
log "RUNTIMED_DEV=${RUNTIMED_DEV:-<unset>}"
log "RUNTIMED_WORKSPACE_PATH=${RUNTIMED_WORKSPACE_PATH:-<unset>}"

# Write pidfile for lifecycle tracking (will be stale after exec)
PIDFILE="$LOGDIR/nteract-mcp.pid"
echo $$ > "$PIDFILE"
trap 'rm -f "$PIDFILE"; log "exiting (pid=$$)"' EXIT

# Use PWD (set by Zed to the project root) as the workspace path
export RUNTIMED_DEV="${RUNTIMED_DEV:-1}"
export RUNTIMED_WORKSPACE_PATH="${RUNTIMED_WORKSPACE_PATH:-$PROJECT_ROOT}"

log "RUNTIMED_WORKSPACE_PATH=$RUNTIMED_WORKSPACE_PATH"
log "exec uv run --no-sync --directory $PROJECT_ROOT/python nteract"

# Don't exec — keep bash alive so the pidfile and trap stay valid.
# Route stderr to the log file so MCP stdio transport stays clean.
uv run --no-sync --directory "$PROJECT_ROOT/python" nteract 2>> "$LOGFILE"
EXIT_CODE=$?
log "nteract exited with code $EXIT_CODE"
exit $EXIT_CODE
