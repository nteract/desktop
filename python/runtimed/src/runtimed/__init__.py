"""runtimed - Python toolkit for Jupyter runtimes."""

from importlib.metadata import PackageNotFoundError, version

from runtimed._cell import CellCollection, CellHandle

# Primary API
from runtimed._client import Client
from runtimed._notebook import Notebook
from runtimed._notebook_info import NotebookInfo
from runtimed._presence import Presence

# Data types (from native bindings)
# These are importable but not in __all__ — for internal consumers
# and power users who need direct access to the native types.
from runtimed.runtimed import (  # noqa: F401
    AsyncSession,
    Cell,
    CompletionItem,
    CompletionResult,
    EnvState,
    ExecutionEvent,
    ExecutionResult,
    HistoryEntry,
    KernelState,
    NativeAsyncClient,
    NativeClient,
    NotebookConnectionInfo,
    Output,
    QueueState,
    RuntimedError,
    RuntimeState,
    Session,
    SyncEnvironmentResult,
    default_socket_path,
    show_notebook_app,
)

__all__ = [
    # Primary API
    "Client",
    "Notebook",
    "NotebookInfo",
    "CellHandle",
    "CellCollection",
    "Presence",
    # Data types (reachable through wrapper)
    "Cell",
    "ExecutionEvent",
    "ExecutionResult",
    "Output",
    "RuntimedError",
    "SyncEnvironmentResult",
    # Runtime state
    "RuntimeState",
    "KernelState",
    "EnvState",
    # Standalone functions
    "default_socket_path",
    "show_notebook_app",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
