"""runtimed - Python toolkit for Jupyter runtimes."""

from importlib.metadata import PackageNotFoundError, version

from runtimed._cell import CellCollection, CellHandle

# Primary API
from runtimed._client import Client
from runtimed._execution import Execution
from runtimed._notebook import Notebook
from runtimed._notebook_info import NotebookInfo
from runtimed._presence import Presence

# Data types (from native bindings)
# These are importable but not in __all__ — they are return-only types
# with no Python constructors. Users encounter them as return values
# (e.g. cell.run() → ExecutionResult, notebook.runtime → RuntimeState)
# but cannot instantiate them directly.
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
    PyQueueEntry,
    QueueState,
    RuntimedError,
    RuntimeState,
    Session,
    SyncEnvironmentResult,
    default_socket_path,
    show_notebook_app,
)

__all__ = [
    # Primary API — constructable entry points
    "Client",
    "Notebook",
    "NotebookInfo",
    "CellHandle",
    "CellCollection",
    "Execution",
    "Presence",
    # Error type — raisable / catchable
    "RuntimedError",
    # Standalone functions
    "default_socket_path",
    "show_notebook_app",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
