"""runtimed - Python toolkit for Jupyter runtimes, powered by the runtimed daemon."""

from importlib.metadata import PackageNotFoundError, version

from runtimed._cell import CellCollection, CellHandle

# Primary API
from runtimed._client import Client
from runtimed._execution import Execution

# Return-only data types (from native bindings)
# Importable for type annotations but not directly constructable.
# Users encounter them as return values from the public API
# (e.g. cell.run() → ExecutionResult, notebook.runtime → RuntimeState).
from runtimed._internals import (  # noqa: F401
    Cell,
    CompletionItem,
    CompletionResult,
    EnvState,
    ExecutionEvent,
    ExecutionResult,
    HistoryEntry,
    KernelState,
    NotebookConnectionInfo,
    Output,
    PyQueueEntry,
    QueueState,
    RuntimedError,
    RuntimeState,
    SyncEnvironmentResult,
    default_socket_path,
    show_notebook_app,
    show_notebook_app_for_channel,
    socket_path_for_channel,
)
from runtimed._notebook import Notebook
from runtimed._notebook_info import NotebookInfo
from runtimed._presence import Presence

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
    "show_notebook_app_for_channel",
    "socket_path_for_channel",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
