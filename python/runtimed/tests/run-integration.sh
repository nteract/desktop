#!/bin/bash
# Integration test runner for runtimed-py
#
# Usage:
#   ./tests/run-integration.sh           # Run with existing dev daemon
#   ./tests/run-integration.sh --ci      # CI mode: spawn own daemon
#   ./tests/run-integration.sh --build   # Build first, then run
#
# Environment variables:
#   RUNTIMED_LOG_LEVEL    - Daemon log level (default: info)
#   RUNTIMED_BINARY       - Path to runtimed binary
#   RUNTIMED_SOCKET_PATH  - Custom socket path (dev mode only)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PYTHON_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$PYTHON_DIR/../.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[test]${NC} $1"; }
warn() { echo -e "${YELLOW}[test]${NC} $1"; }
error() { echo -e "${RED}[test]${NC} $1"; }

# Parse arguments
CI_MODE=0
BUILD_FIRST=0
PYTEST_ARGS=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --ci)
            CI_MODE=1
            shift
            ;;
        --build)
            BUILD_FIRST=1
            shift
            ;;
        *)
            PYTEST_ARGS="$PYTEST_ARGS $1"
            shift
            ;;
    esac
done

# Enable worktree isolation (same as e2e tests)
export RUNTIMED_WORKSPACE_PATH="$PROJECT_ROOT"

cd "$PYTHON_DIR"

# Build if requested
if [ "$BUILD_FIRST" = "1" ]; then
    log "Building runtimed binary..."
    cargo build -p runtimed --manifest-path "$PROJECT_ROOT/Cargo.toml"

    log "Building runtimed-py..."
    uv run maturin develop --manifest-path "$PROJECT_ROOT/crates/runtimed-py/Cargo.toml"
fi

# Check if binary exists for CI mode
if [ "$CI_MODE" = "1" ]; then
    BINARY="${RUNTIMED_BINARY:-$PROJECT_ROOT/target/debug/runtimed}"
    if [ ! -f "$BINARY" ]; then
        error "runtimed binary not found at $BINARY"
        error "Build with: cargo build -p runtimed"
        exit 1
    fi
    export RUNTIMED_BINARY="$BINARY"
    export RUNTIMED_INTEGRATION_TEST=1
    log "CI mode: will spawn daemon from $BINARY"
else
    # Dev mode: check if daemon is running
    log "Dev mode: checking for running daemon..."

    # Try to import runtimed and check socket
    if ! uv run python -c "
import runtimed
import os
path = runtimed.default_socket_path() if hasattr(runtimed, 'default_socket_path') else None
if path and not os.path.exists(path):
    print(f'Socket not found: {path}')
    exit(1)
print(f'Using socket: {path}')
" 2>/dev/null; then
        warn "Daemon may not be running. Start with: cargo xtask dev-daemon"
        warn "Or run in CI mode: ./tests/run-integration.sh --ci"
    fi
fi

# Run tests
log "Running integration tests..."
PYTEST_DEFAULT_ARGS="-v --tb=short"

if [ -z "$PYTEST_ARGS" ]; then
    # Default: run daemon integration tests
    PYTEST_ARGS="tests/test_daemon_integration.py"
fi

exec uv run pytest $PYTEST_DEFAULT_ARGS $PYTEST_ARGS
