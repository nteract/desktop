"""Type stubs for the runtimed native extension module (PyO3).

Auto-maintained — regenerate from crates/runtimed-py/src/ when the Rust API changes.
"""

from __future__ import annotations

import os
from collections.abc import Coroutine
from typing import Any

# ---------------------------------------------------------------------------
# Exception
# ---------------------------------------------------------------------------

class RuntimedError(Exception):
    """Error raised by runtimed operations."""

    ...

# ---------------------------------------------------------------------------
# Data classes (output types)
# ---------------------------------------------------------------------------

class Output:
    """A single output from cell execution."""

    @property
    def output_type(self) -> str:
        """Output type: "stream", "display_data", "execute_result", "error"."""
        ...

    @property
    def name(self) -> str | None:
        """For stream outputs: "stdout" or "stderr"."""
        ...

    @property
    def text(self) -> str | None:
        """For stream outputs: the text content."""
        ...

    @property
    def data(self) -> dict[str, str | bytes | dict[str, Any] | list[Any]] | None:
        """For display_data/execute_result: mime type -> typed content.

        Values are typed by MIME category:
        - Text mimes (text/*, image/svg+xml) → ``str``
        - Binary mimes (image/png, audio/*, …) → ``bytes`` (raw, not base64)
        - JSON mimes (application/json, *+json) → ``dict`` (typically),
          or ``list`` in rare cases. The Jupyter protocol specifies JSON
          MIME data as objects, but the implementation accepts any valid
          JSON value for robustness.
        """
        ...

    @property
    def ename(self) -> str | None:
        """For errors: exception name."""
        ...

    @property
    def evalue(self) -> str | None:
        """For errors: exception value."""
        ...

    @property
    def traceback(self) -> list[str] | None:
        """For errors: traceback lines."""
        ...

    @property
    def execution_count(self) -> int | None:
        """For execute_result: execution count."""
        ...

    @property
    def blob_urls(self) -> dict[str, str] | None:
        """For display_data/execute_result: MIME type → blob HTTP URL.

        Only present for outputs that have blob-stored data.
        """
        ...

    @property
    def blob_paths(self) -> dict[str, str] | None:
        """For display_data/execute_result: MIME type → on-disk file path.

        Only present for outputs that have blob-stored data.
        """
        ...

class Cell:
    """A cell from the automerge document."""

    @property
    def id(self) -> str: ...
    @property
    def cell_type(self) -> str:
        """Cell type: "code", "markdown", or "raw"."""
        ...

    @property
    def position(self) -> str:
        """Fractional index hex string for ordering."""
        ...

    @property
    def source(self) -> str:
        """Cell source code/content."""
        ...

    @property
    def execution_count(self) -> int | None: ...
    @property
    def outputs(self) -> list[Output]: ...
    @property
    def metadata_json(self) -> str:
        """Cell metadata as JSON string."""
        ...

    @property
    def metadata(self) -> Any:
        """Parsed metadata dict."""
        ...

    @property
    def is_source_hidden(self) -> bool: ...
    @property
    def is_outputs_hidden(self) -> bool: ...
    @property
    def is_collapsed(self) -> bool: ...
    @property
    def tags(self) -> list[str]: ...

class ExecutionEvent:
    """An event from streaming cell execution."""

    @property
    def event_type(self) -> str:
        """Event type: "execution_started", "output", "done", "error", "kernel_status"."""
        ...

    @property
    def cell_id(self) -> str: ...
    @property
    def output(self) -> Output | None:
        """The output (only for "output" events, None in signal-only mode)."""
        ...

    @property
    def output_index(self) -> int | None:
        """Index of the output in the cell's outputs list."""
        ...

    @property
    def execution_count(self) -> int | None:
        """Execution count (only for "execution_started" events)."""
        ...

    @property
    def error_message(self) -> str | None:
        """Error message (only for "error" events)."""
        ...

class ExecutionResult:
    """Result of executing code."""

    @property
    def cell_id(self) -> str: ...
    @property
    def outputs(self) -> list[Output]: ...
    @property
    def success(self) -> bool: ...
    @property
    def execution_count(self) -> int | None: ...
    @property
    def stdout(self) -> str:
        """Combined stdout text."""
        ...

    @property
    def stderr(self) -> str:
        """Combined stderr text."""
        ...

    @property
    def display_data(self) -> list[Output]:
        """Display data outputs (display_data and execute_result)."""
        ...

    @property
    def error(self) -> Output | None:
        """Error output if any."""
        ...

class CompletionItem:
    """A single completion item from the kernel."""

    @property
    def label(self) -> str: ...
    @property
    def kind(self) -> str | None:
        """Kind: "function", "variable", "class", "module", etc."""
        ...

    @property
    def detail(self) -> str | None:
        """Short type annotation."""
        ...

    @property
    def source(self) -> str | None:
        """Source: "kernel"."""
        ...

class CompletionResult:
    """Result of a code completion request."""

    @property
    def items(self) -> list[CompletionItem]: ...
    @property
    def cursor_start(self) -> int: ...
    @property
    def cursor_end(self) -> int: ...

class PyQueueEntry:
    """An entry in the execution queue."""

    @property
    def cell_id(self) -> str:
        """Cell ID."""
        ...

    @property
    def execution_id(self) -> str:
        """Execution ID (UUID)."""
        ...

class QueueState:
    """Current state of the execution queue."""

    @property
    def executing(self) -> PyQueueEntry | None:
        """Entry currently executing (None if idle)."""
        ...

    @property
    def queued(self) -> list[PyQueueEntry]:
        """Entries waiting in queue."""
        ...

class KernelState:
    """Kernel state from the RuntimeStateDoc."""

    @property
    def status(self) -> str:
        """Kernel status: "not_started", "starting", "idle", "busy", "error", "shutdown"."""
        ...
    @property
    def name(self) -> str:
        """Kernel display name (e.g. "charming-toucan")."""
        ...
    @property
    def language(self) -> str:
        """Kernel language (e.g. "python", "typescript")."""
        ...
    @property
    def env_source(self) -> str:
        """Environment source label (e.g. "uv:prewarmed", "pixi:toml")."""
        ...

class EnvState:
    """Environment sync state from the RuntimeStateDoc."""

    @property
    def in_sync(self) -> bool:
        """Whether notebook metadata matches the launched kernel config."""
        ...
    @property
    def added(self) -> list[str]:
        """Packages in metadata but not in the kernel environment."""
        ...
    @property
    def removed(self) -> list[str]:
        """Packages in the kernel environment but not in metadata."""
        ...
    @property
    def channels_changed(self) -> bool:
        """Whether conda channels differ."""
        ...
    @property
    def deno_changed(self) -> bool:
        """Whether deno config differs."""
        ...
    @property
    def prewarmed_packages(self) -> list[str]:
        """Packages pre-installed in the prewarmed environment."""
        ...

class ExecutionState:
    """Execution lifecycle state for a single execution."""

    @property
    def cell_id(self) -> str:
        """Cell that was executed."""
        ...

    @property
    def status(self) -> str:
        """Current status: 'queued', 'running', 'done', 'error'."""
        ...

    @property
    def execution_count(self) -> int | None:
        """Kernel execution count (None if not yet started)."""
        ...

    @property
    def success(self) -> bool | None:
        """Whether the execution succeeded (None if still running)."""
        ...

class RuntimeState:
    """Full runtime state snapshot from the daemon's RuntimeStateDoc."""

    @property
    def kernel(self) -> KernelState:
        """Kernel state (status, name, language, env_source)."""
        ...
    @property
    def queue(self) -> QueueState:
        """Execution queue state."""
        ...
    @property
    def env(self) -> EnvState:
        """Environment sync state."""
        ...
    @property
    def last_saved(self) -> str | None:
        """ISO timestamp of last save, or None."""
        ...
    @property
    def executions(self) -> dict[str, ExecutionState]:
        """Execution lifecycle entries keyed by execution_id."""
        ...

class HistoryEntry:
    """A single entry from kernel input history."""

    @property
    def session(self) -> int: ...
    @property
    def line(self) -> int: ...
    @property
    def source(self) -> str: ...

class SyncEnvironmentResult:
    """Result of syncing environment with metadata."""

    @property
    def success(self) -> bool: ...
    @property
    def synced_packages(self) -> list[str]: ...
    @property
    def error(self) -> str | None: ...
    @property
    def needs_restart(self) -> bool: ...

class NotebookConnectionInfo:
    """Connection info returned when opening or creating a notebook."""

    @property
    def protocol(self) -> str: ...
    @property
    def protocol_version(self) -> int | None: ...
    @property
    def daemon_version(self) -> str | None: ...
    @property
    def notebook_id(self) -> str: ...
    @property
    def cell_count(self) -> int: ...
    @property
    def needs_trust_approval(self) -> bool: ...

# ---------------------------------------------------------------------------
# AsyncClient
# ---------------------------------------------------------------------------

class NativeAsyncClient:
    """Async native client for the runtimed daemon.

    Low-level client — the Python ``runtimed.Client`` wraps this to return
    ``Notebook`` objects instead of raw ``AsyncSession``.
    """

    def __init__(
        self,
        socket_path: str | None = None,
        peer_label: str | None = None,
    ) -> None: ...
    def ping(self) -> Coroutine[Any, Any, bool]: ...
    def is_running(self) -> Coroutine[Any, Any, bool]: ...
    def status(self) -> Coroutine[Any, Any, dict[str, Any]]: ...
    def list_active_notebooks(self) -> Coroutine[Any, Any, list[dict[str, Any]]]: ...
    def flush_pool(self) -> Coroutine[Any, Any, None]: ...
    def shutdown(self) -> Coroutine[Any, Any, None]: ...
    def open_notebook(
        self,
        path: str,
        peer_label: str | None = None,
    ) -> Coroutine[Any, Any, AsyncSession]: ...
    def create_notebook(
        self,
        runtime: str = "python",
        working_dir: str | None = None,
        peer_label: str | None = None,
    ) -> Coroutine[Any, Any, AsyncSession]: ...
    def join_notebook(
        self,
        notebook_id: str,
        peer_label: str | None = None,
    ) -> Coroutine[Any, Any, AsyncSession]: ...

# ---------------------------------------------------------------------------
# AsyncSession
# ---------------------------------------------------------------------------

class AsyncSession:
    """Async session for notebook interaction."""

    @property
    def notebook_id(self) -> str: ...
    def blob_base_url(self) -> Coroutine[Any, Any, str | None]: ...
    def blob_store_path(self) -> Coroutine[Any, Any, str | None]: ...
    def is_connected(self) -> Coroutine[Any, Any, bool]: ...
    def kernel_started(self) -> Coroutine[Any, Any, bool]: ...
    def kernel_type(self) -> Coroutine[Any, Any, str | None]: ...
    def env_source(self) -> Coroutine[Any, Any, str | None]: ...
    def connection_info(self) -> Coroutine[Any, Any, NotebookConnectionInfo | None]: ...
    def connect(self) -> Coroutine[Any, Any, None]: ...

    # Kernel lifecycle
    def start_kernel(
        self,
        kernel_type: str = "python",
        env_source: str = "auto",
        notebook_path: str | None = None,
    ) -> Coroutine[Any, Any, None]: ...
    def shutdown_kernel(self) -> Coroutine[Any, Any, None]: ...
    def restart_kernel(self, wait_for_ready: bool = True) -> Coroutine[Any, Any, list[str]]: ...
    def interrupt(self) -> Coroutine[Any, Any, None]: ...

    # Cell operations
    def create_cell(
        self,
        source: str = "",
        cell_type: str = "code",
        index: int | None = None,
    ) -> Coroutine[Any, Any, str]:
        """Create a new cell.

        Args:
            source: Cell source text.
            cell_type: One of "code", "markdown", "raw".
            index: Position to insert. ``None`` appends at the end,
                ``0`` prepends at the beginning.

        Returns:
            The new cell's ID.
        """
        ...
    def set_source(self, cell_id: str, source: str) -> Coroutine[Any, Any, None]: ...
    def splice_source(
        self, cell_id: str, index: int, delete_count: int, text: str
    ) -> Coroutine[Any, Any, None]:
        """Splice a cell's source at a specific position (character-level, no diff).

        Deletes ``delete_count`` characters starting at ``index``, then inserts ``text``.
        This is the fast path for surgical edits — no Myers diff overhead.
        """
        ...
    def append_source(self, cell_id: str, text: str) -> Coroutine[Any, Any, None]: ...
    def set_cell_type(self, cell_id: str, cell_type: str) -> Coroutine[Any, Any, None]: ...
    def get_cell(self, cell_id: str) -> Coroutine[Any, Any, Cell]: ...
    def get_cells(self) -> Coroutine[Any, Any, list[Cell]]: ...
    def get_cell_source(self, cell_id: str) -> Coroutine[Any, Any, str | None]: ...
    def get_cell_type(self, cell_id: str) -> Coroutine[Any, Any, str | None]: ...
    def get_cell_outputs(self, cell_id: str) -> Coroutine[Any, Any, list[str] | None]: ...
    def get_cell_execution_count(self, cell_id: str) -> Coroutine[Any, Any, str | None]: ...
    def get_cell_ids(self) -> Coroutine[Any, Any, list[str]]: ...
    def get_cell_position(self, cell_id: str) -> Coroutine[Any, Any, str | None]: ...
    def delete_cell(self, cell_id: str) -> Coroutine[Any, Any, None]: ...
    def move_cell(
        self, cell_id: str, after_cell_id: str | None = None
    ) -> Coroutine[Any, Any, str]: ...
    def clear_outputs(self, cell_id: str) -> Coroutine[Any, Any, None]: ...

    # Presence
    def get_peers(self) -> Coroutine[Any, Any, list[tuple[str, str]]]: ...
    def get_remote_cursors(
        self,
    ) -> Coroutine[Any, Any, list[tuple[str, str, str, int, int]]]: ...
    def set_cursor(self, cell_id: str, line: int, column: int) -> Coroutine[Any, Any, None]: ...
    def set_selection(
        self,
        cell_id: str,
        anchor_line: int,
        anchor_col: int,
        head_line: int,
        head_col: int,
    ) -> Coroutine[Any, Any, None]: ...
    def set_focus(self, cell_id: str) -> Coroutine[Any, Any, None]: ...
    def clear_cursor(self) -> Coroutine[Any, Any, None]: ...
    def clear_selection(self) -> Coroutine[Any, Any, None]: ...

    # Save / Metadata
    def save(self, path: str | None = None) -> Coroutine[Any, Any, str]: ...
    def set_metadata(self, key: str, value: str) -> Coroutine[Any, Any, None]: ...
    def get_metadata(self, key: str) -> Coroutine[Any, Any, str | None]: ...
    def set_kernelspec(
        self,
        name: str,
        display_name: str,
        language: str | None = None,
    ) -> Coroutine[Any, Any, None]: ...
    def get_kernelspec(self) -> Coroutine[Any, Any, dict[str, str] | None]: ...

    # Cell metadata
    def get_cell_metadata(self, cell_id: str) -> Coroutine[Any, Any, str | None]: ...
    def set_cell_metadata(self, cell_id: str, metadata_json: str) -> Coroutine[Any, Any, bool]: ...
    def update_cell_metadata_at(
        self,
        cell_id: str,
        path: list[str],
        value_json: str,
    ) -> Coroutine[Any, Any, bool]: ...
    def set_cell_source_hidden(self, cell_id: str, hidden: bool) -> Coroutine[Any, Any, bool]: ...
    def set_cell_outputs_hidden(self, cell_id: str, hidden: bool) -> Coroutine[Any, Any, bool]: ...
    def set_cell_tags(self, cell_id: str, tags: list[str]) -> Coroutine[Any, Any, bool]: ...

    # Dependencies
    def get_uv_dependencies(self) -> Coroutine[Any, Any, list[str]]: ...
    def add_uv_dependency(self, package: str) -> Coroutine[Any, Any, None]: ...
    def add_uv_dependencies(self, packages: list[str]) -> Coroutine[Any, Any, None]: ...
    def remove_uv_dependency(self, package: str) -> Coroutine[Any, Any, bool]: ...
    def get_conda_dependencies(self) -> Coroutine[Any, Any, list[str]]: ...
    def add_conda_dependency(self, package: str) -> Coroutine[Any, Any, None]: ...
    def add_conda_dependencies(self, packages: list[str]) -> Coroutine[Any, Any, None]: ...
    def remove_conda_dependency(self, package: str) -> Coroutine[Any, Any, bool]: ...
    def get_metadata_env_type(self) -> Coroutine[Any, Any, str | None]: ...
    def get_settings(self) -> dict[str, Any] | None: ...

    # Execution
    def execute_cell(
        self,
        cell_id: str,
        timeout_secs: float = 60.0,
    ) -> Coroutine[Any, Any, ExecutionResult]: ...
    def queue_cell(self, cell_id: str) -> Coroutine[Any, Any, str]: ...
    def wait_for_execution(
        self,
        cell_id: str,
        execution_id: str,
        timeout_secs: float = 60.0,
    ) -> Coroutine[Any, Any, ExecutionResult]: ...

    # Environment sync
    def sync_environment(self) -> Coroutine[Any, Any, SyncEnvironmentResult]: ...

    # Completion, history, queue
    def complete(self, code: str, cursor_pos: int) -> Coroutine[Any, Any, CompletionResult]: ...
    def get_history(
        self,
        pattern: str | None = None,
        n: int = 100,
        unique: bool = True,
    ) -> Coroutine[Any, Any, list[HistoryEntry]]: ...
    def get_queue_state(self) -> Coroutine[Any, Any, QueueState]: ...
    def get_runtime_state(self) -> Coroutine[Any, Any, RuntimeState]: ...
    def run_all_cells(self) -> Coroutine[Any, Any, int]: ...

    # Synchronous reads (for Python wrapper's sync properties)
    def get_cell_ids_sync(self) -> list[str]: ...
    def get_cell_source_sync(self, cell_id: str) -> str | None: ...
    def get_cell_type_sync(self, cell_id: str) -> str | None: ...
    def get_cell_execution_count_sync(self, cell_id: str) -> str | None: ...
    def get_cell_sync(self, cell_id: str) -> Cell: ...
    def get_cells_sync(self) -> list[Cell]: ...
    def get_cell_metadata_sync(self, cell_id: str) -> str | None: ...
    def get_runtime_state_sync(self) -> RuntimeState: ...
    def get_peers_sync(self) -> list[tuple[str, str]]: ...
    def is_connected_sync(self) -> bool: ...

    # Context manager
    def close(self) -> Coroutine[Any, Any, None]: ...
    async def __aenter__(self) -> AsyncSession: ...
    async def __aexit__(
        self,
        _exc_type: type[BaseException] | None = None,
        _exc_val: BaseException | None = None,
        _exc_tb: Any = None,
    ) -> bool: ...

# ---------------------------------------------------------------------------
# Standalone functions
# ---------------------------------------------------------------------------

def show_notebook_app(
    notebook_path: str | os.PathLike[str] | None = None,
) -> None:
    """Launch the desktop notebook app, optionally opening a specific notebook."""
    ...

def default_socket_path() -> str:
    """Get the default daemon socket path.

    Respects the RUNTIMED_SOCKET_PATH environment variable if set.
    """
    ...

def socket_path_for_channel(channel: str) -> str:
    """Get the daemon socket path for a specific channel ("stable" or "nightly").

    Unlike ``default_socket_path()``, this ignores ``RUNTIMED_SOCKET_PATH``
    and returns the platform-correct path for the requested channel.

    Raises:
        ValueError: If *channel* is not ``"stable"`` or ``"nightly"``.
    """
    ...

def show_notebook_app_for_channel(
    channel: str,
    notebook_path: str | os.PathLike[str] | None = None,
) -> None:
    """Launch the desktop app for a specific channel ("stable" or "nightly").

    Raises:
        ValueError: If *channel* is not ``"stable"`` or ``"nightly"``.
        RuntimeError: If the app could not be launched.
    """
    ...
