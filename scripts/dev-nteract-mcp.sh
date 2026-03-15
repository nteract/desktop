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

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Write pidfile so we can identify this process later
PIDFILE="$PROJECT_ROOT/.context/nteract-mcp.pid"
mkdir -p "$(dirname "$PIDFILE")"
echo $$ > "$PIDFILE"
trap 'rm -f "$PIDFILE"' EXIT

# Use PWD (set by Zed to the project root) as the workspace path
export RUNTIMED_DEV="${RUNTIMED_DEV:-1}"
export RUNTIMED_WORKSPACE_PATH="${RUNTIMED_WORKSPACE_PATH:-$PROJECT_ROOT}"

exec uv run --no-sync --directory "$PROJECT_ROOT/python" nteract
