"""runtimed - Python toolkit for Jupyter runtimes."""

from importlib.metadata import PackageNotFoundError, version

# Native daemon client (PyO3 bindings)
from runtimed.runtimed import (
    AsyncSession,
    Cell,
    CompletionItem,
    CompletionResult,
    DaemonClient,
    ExecutionEvent,
    ExecutionResult,
    HistoryEntry,
    NotebookConnectionInfo,
    Output,
    QueueState,
    RuntimedError,
    Session,
    default_socket_path,
    show_notebook_app,
)

__all__ = [
    # Daemon client API - sync
    "DaemonClient",
    "Session",
    # Daemon client API - async
    "AsyncSession",
    # Output types
    "Cell",
    "ExecutionEvent",
    "ExecutionResult",
    "NotebookConnectionInfo",
    "Output",
    "RuntimedError",
    # Completion and queue types
    "CompletionItem",
    "CompletionResult",
    "QueueState",
    "HistoryEntry",
    # Standalone functions
    "default_socket_path",
    "show_notebook_app",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
