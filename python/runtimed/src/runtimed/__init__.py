"""runtimed - Python toolkit for Jupyter runtimes."""

from importlib.metadata import PackageNotFoundError, version

# Existing sidecar functionality
from runtimed._sidecar import BridgedSidecar, Sidecar, sidecar

# Native daemon client (PyO3 bindings)
from runtimed.runtimed import (
    AsyncSession,
    Cell,
    DaemonClient,
    ExecutionEvent,
    ExecutionResult,
    Output,
    RuntimedError,
    Session,
)

__all__ = [
    # Existing
    "BridgedSidecar",
    "Sidecar",
    "sidecar",
    # Daemon client API - sync
    "DaemonClient",
    "Session",
    # Daemon client API - async
    "AsyncSession",
    # Output types
    "Cell",
    "ExecutionEvent",
    "ExecutionResult",
    "Output",
    "RuntimedError",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
