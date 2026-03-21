"""runtimed - Python toolkit for Jupyter runtimes."""

from importlib.metadata import PackageNotFoundError, version

from runtimed._cell import CellCollection, CellHandle

# Primary API
from runtimed._client import Client
from runtimed._notebook import Notebook
from runtimed._notebook_info import NotebookInfo

# Data types (from native bindings)
# Runtime state types (renamed from Py* to clean names)
# Standalone functions
# Native types (for advanced use / internal consumers like nteract MCP)
from runtimed.runtimed import (
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
    # Data types
    "Cell",
    "CompletionItem",
    "CompletionResult",
    "ExecutionEvent",
    "ExecutionResult",
    "HistoryEntry",
    "NotebookConnectionInfo",
    "Output",
    "QueueState",
    "RuntimedError",
    "SyncEnvironmentResult",
    # Runtime state
    "RuntimeState",
    "KernelState",
    "EnvState",
    # Standalone functions
    "default_socket_path",
    "show_notebook_app",
    # Native types (advanced)
    "NativeAsyncClient",
    "NativeClient",
    "AsyncSession",
    "Session",
]

try:
    __version__ = version("runtimed")
except PackageNotFoundError:
    __version__ = "0.0.0-dev"
