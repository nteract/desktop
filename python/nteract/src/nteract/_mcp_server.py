"""nteract MCP server for AI-powered Jupyter notebooks.

This server exposes notebook operations as MCP tools, allowing AI agents
to create cells, execute code, and read outputs. For realtime sync with
users, use the nteract desktop app connected to the same notebook.

Usage:
    python -m nteract._mcp_server

Or via the entry point:
    nteract

Requires: pip install nteract
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import re
import sys
from typing import Any

import runtimed
from mcp.server.fastmcp import FastMCP
from mcp.types import ImageContent, TextContent, ToolAnnotations

logger = logging.getLogger(__name__)

# MCP content types for tool responses
ContentItem = TextContent | ImageContent

# Create the MCP server
mcp = FastMCP("nteract")

# Session state - single active session at a time
_session: runtimed.AsyncSession | None = None
_daemon_client: runtimed.DaemonClient | None = None


def _get_daemon_client() -> runtimed.DaemonClient:
    """Get or create the daemon client."""
    global _daemon_client
    if _daemon_client is None:
        _daemon_client = runtimed.DaemonClient()
    return _daemon_client


async def _get_session() -> runtimed.AsyncSession:
    """Get the current session, raising error if not connected."""
    if _session is None:
        raise RuntimeError("No active notebook session. Call connect_notebook first.")
    return _session


# Regex to strip ANSI escape sequences (terminal colors, cursor movement, etc.)
_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B")


def _strip_ansi(text: str) -> str:
    """Strip ANSI escape sequences from text.

    Kernel stream output (especially from pip/uv installs) often contains
    terminal control codes for colors, progress bars, and cursor movement.
    These waste LLM context and render as garbage in text responses.
    """
    return _ANSI_RE.sub("", text)


# Maximum size for image data (base64-encoded). 1 MB is generous — a typical
# matplotlib PNG is 50–100 KB. Images beyond this are silently dropped to
# avoid blowing up the LLM's context window.
_MAX_IMAGE_BASE64_BYTES = 1_000_000

# Text mime type priority for LLM consumption.
# text/llm+plain is from https://github.com/rgbkrk/repr_llm — a repr designed
# specifically for language models. text/html is intentionally excluded: it's
# often bulky embedded JS (e.g. Plotly) that wastes context window.
_TEXT_MIME_PRIORITY = (
    "text/llm+plain",
    "text/markdown",
    "text/plain",
    "application/json",
)


def _format_output_text(output: runtimed.Output) -> str | None:
    """Extract text representation from a single output.

    Returns the best text representation, or None if no text available.
    Priority: text/llm+plain > text/markdown > text/plain > application/json
    """
    if output.output_type == "stream":
        return _strip_ansi(output.text) if output.text else None

    if output.output_type == "error":
        parts = []
        if output.ename and output.evalue:
            parts.append(f"{output.ename}: {output.evalue}")
        elif output.evalue:
            parts.append(output.evalue)
        if output.traceback:
            parts.append("\n".join(output.traceback))
        return _strip_ansi("\n".join(parts)) if parts else None

    if output.output_type in ("display_data", "execute_result"):
        if output.data is None:
            return None
        for mime in _TEXT_MIME_PRIORITY:
            if mime not in output.data:
                continue
            if mime == "application/json":
                try:
                    data = output.data[mime]
                    if isinstance(data, str):
                        return json.dumps(json.loads(data), indent=2)
                    return json.dumps(data, indent=2)
                except (json.JSONDecodeError, TypeError):
                    return str(output.data[mime])
            return output.data[mime]
        return None

    return None


def _format_outputs_text(outputs: list[runtimed.Output]) -> str:
    """Convert a list of outputs to readable text.

    Extracts only text-based representations. Ignores images, HTML, and
    other binary/bulky formats.
    """
    parts: list[str] = []
    for output in outputs:
        text = _format_output_text(output)
        if text:
            parts.append(text)
    return "\n\n".join(parts)


def _output_to_content(output: runtimed.Output) -> list[ContentItem]:
    """Convert a single output to a list of MCP content items.

    Returns the richest representation for each mime type:
    - image/png, image/jpeg, image/gif, image/webp → ImageContent
    - image/svg+xml → TextContent (XML text, not base64)
    - text/llm+plain, text/markdown, text/plain, application/json → TextContent
    - stream, error → TextContent

    text/html is intentionally excluded — it's often bulky embedded JS
    (e.g. Plotly, Bokeh) that wastes LLM context window.
    """
    items: list[ContentItem] = []

    if output.output_type == "stream":
        if output.text:
            cleaned = _strip_ansi(output.text)
            if cleaned.strip():
                items.append(TextContent(type="text", text=cleaned))
        return items

    if output.output_type == "error":
        parts = []
        if output.ename and output.evalue:
            parts.append(f"{output.ename}: {output.evalue}")
        elif output.evalue:
            parts.append(output.evalue)
        if output.traceback:
            parts.append("\n".join(output.traceback))
        if parts:
            items.append(TextContent(type="text", text=_strip_ansi("\n".join(parts))))
        return items

    if output.output_type in ("display_data", "execute_result"):
        if output.data is None:
            return items

        # Images → ImageContent (base64 encoded by the kernel)
        for mime in ("image/png", "image/jpeg", "image/gif", "image/webp"):
            if mime in output.data:
                data = output.data[mime]
                if isinstance(data, str) and len(data) <= _MAX_IMAGE_BASE64_BYTES:
                    items.append(ImageContent(type="image", data=data, mimeType=mime))

        # SVG as text (it's XML, not base64)
        if "image/svg+xml" in output.data:
            items.append(TextContent(type="text", text=output.data["image/svg+xml"]))

        # Best available text representation
        for mime in _TEXT_MIME_PRIORITY:
            if mime not in output.data:
                continue
            if mime == "application/json":
                try:
                    data = output.data[mime]
                    if isinstance(data, str):
                        text = json.dumps(json.loads(data), indent=2)
                    else:
                        text = json.dumps(data, indent=2)
                    items.append(TextContent(type="text", text=text))
                except (json.JSONDecodeError, TypeError):
                    items.append(TextContent(type="text", text=str(output.data[mime])))
            else:
                items.append(TextContent(type="text", text=output.data[mime]))
            break

    return items


def _outputs_to_content(outputs: list[runtimed.Output]) -> list[ContentItem]:
    """Convert a list of outputs to MCP content items.

    Each output may produce multiple items (e.g. an image + its text/plain alt).
    """
    items: list[ContentItem] = []
    for output in outputs:
        items.extend(_output_to_content(output))
    return items


def _format_header(
    cell_id: str,
    status: str | None = None,
    execution_count: int | None = None,
) -> str:
    """Format a cell header line for terminal display.

    Example: ━━━ cell-abc12345 ✓ idle [3] ━━━
    """
    icons = {"idle": "✓", "error": "✗", "running": "◐"}

    parts = [f"━━━ {cell_id}"]

    if status:
        icon = icons.get(status, "?")
        parts.append(f"{icon} {status}")

    if execution_count is not None:
        parts.append(f"[{execution_count}]")

    parts.append("━━━")
    return " ".join(parts)


def _format_cell(cell: runtimed.Cell) -> str:
    """Format a cell for terminal display (includes source).

    Used by get_cell to show full cell state.
    """
    header = _format_header(cell.id, execution_count=cell.execution_count)
    output_text = _format_outputs_text(cell.outputs)

    if cell.source and output_text:
        return f"{header}\n\n{cell.source}\n\n───────────────────\n\n{output_text}"
    elif cell.source:
        return f"{header}\n\n{cell.source}"
    elif output_text:
        return f"{header}\n\n{output_text}"
    else:
        return header


def _cell_to_content(cell: runtimed.Cell) -> list[ContentItem]:
    """Convert a cell to rich MCP content items.

    Returns a header as TextContent, then each output as its richest type.
    """
    header = _format_header(cell.id, execution_count=cell.execution_count)
    items: list[ContentItem] = []

    if cell.source:
        items.append(TextContent(type="text", text=f"{header}\n\n{cell.source}"))
    else:
        items.append(TextContent(type="text", text=header))

    output_items = _outputs_to_content(cell.outputs)
    if output_items:
        items.extend(output_items)

    return items


def _format_execution_result(
    cell_id: str,
    events: list[Any],  # list[runtimed.ExecutionEvent]
    complete: bool,
) -> str:
    """Format execution result for terminal display.

    Status reflects execution outcome:
    - "running": execution in progress (complete=false)
    - "idle": completed successfully
    - "error": execution raised an exception
    """
    outputs: list[runtimed.Output] = []
    execution_count: int | None = None
    status = "running"
    has_error_output = False

    for event in events:
        if event.event_type == "execution_started":
            execution_count = event.execution_count
        elif event.event_type == "output":
            outputs.append(event.output)
            if event.output.output_type == "error":
                has_error_output = True
        elif event.event_type == "done":
            status = "error" if has_error_output else "idle"
        elif event.event_type == "error":
            status = "error"

    header = _format_header(cell_id, status=status, execution_count=execution_count)
    output_text = _format_outputs_text(outputs)

    if output_text:
        return f"{header}\n\n{output_text}"
    elif not complete:
        return f"{header}\n\n(execution in progress...)"
    else:
        return header


def _execution_result_to_content(
    cell_id: str,
    events: list[Any],  # list[runtimed.ExecutionEvent]
    complete: bool,
) -> list[ContentItem]:
    """Convert execution result to rich MCP content items.

    Returns a header TextContent, then each output as its richest type.
    """
    outputs: list[runtimed.Output] = []
    execution_count: int | None = None
    status = "running"
    has_error_output = False

    for event in events:
        if event.event_type == "execution_started":
            execution_count = event.execution_count
        elif event.event_type == "output":
            outputs.append(event.output)
            if event.output.output_type == "error":
                has_error_output = True
        elif event.event_type == "done":
            status = "error" if has_error_output else "idle"
        elif event.event_type == "error":
            status = "error"

    header = _format_header(cell_id, status=status, execution_count=execution_count)
    items: list[ContentItem] = [TextContent(type="text", text=header)]

    output_items = _outputs_to_content(outputs)
    if output_items:
        items.extend(output_items)
    elif not complete:
        items.append(TextContent(type="text", text="(execution in progress...)"))

    return items


# =============================================================================
# Session Management Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def connect_notebook(
    notebook_id: str | None = None,
) -> dict[str, Any]:
    """Connect to a notebook session.

    Creates or connects to a notebook session in the daemon. Multiple calls
    with the same notebook_id will share the same kernel.

    Args:
        notebook_id: Optional ID for the notebook. If not provided, generates
            a unique ID. Use the same ID to reconnect to an existing session.

    Returns:
        Session info including the notebook_id.
    """
    global _session

    # Close existing session if any
    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    # Create new session
    _session = runtimed.AsyncSession(notebook_id=notebook_id)
    await _session.connect()

    return {
        "notebook_id": _session.notebook_id,
        "connected": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def disconnect_notebook() -> dict[str, Any]:
    """Disconnect from the current notebook session.

    Closes the connection but does not shutdown the kernel (other clients
    may still be using it).

    Returns:
        Confirmation of disconnection.
    """
    global _session

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()
        _session = None

    return {"disconnected": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def open_notebook(path: str) -> dict[str, Any]:
    """Open an existing .ipynb notebook file from disk.

    Use this to open a notebook that already exists on the filesystem.
    For creating a new notebook, use create_notebook() instead.

    Args:
        path: Absolute or relative path to the .ipynb file.

    Returns:
        Connection info including notebook_id and trust status.
    """
    global _session

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.open_notebook(path)
    info = await _session.connection_info()
    return {
        "notebook_id": _session.notebook_id,
        "path": path,
        "cell_count": info.cell_count if info else 0,
        "needs_trust_approval": info.needs_trust_approval if info else False,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def create_notebook(
    runtime: str = "python",
    working_dir: str | None = None,
) -> dict[str, Any]:
    """Create a new empty notebook in memory.

    Creates an empty notebook with one code cell via the daemon. The notebook
    exists only in memory until saved with save_notebook(path).

    NOTE: This tool does NOT accept a path parameter. To save the notebook
    to disk, call save_notebook(path) after creating it.

    Args:
        runtime: Kernel runtime type ("python" or "deno").
        working_dir: Optional working directory for project detection.

    Returns:
        Connection info including notebook_id (a session identifier, not a file path).
    """
    global _session

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.create_notebook(runtime=runtime, working_dir=working_dir)
    info = await _session.connection_info()
    return {
        "notebook_id": _session.notebook_id,
        "runtime": runtime,
        "cell_count": info.cell_count if info else 1,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def save_notebook(path: str | None = None) -> dict[str, Any]:
    """Save the current notebook to disk as a .ipynb file.

    Persists the current notebook state including cells, outputs, and metadata.

    Args:
        path: File path to save to. REQUIRED for notebooks created with
            create_notebook(). Can be omitted for notebooks opened with
            open_notebook() to save to the original location.

    Returns:
        The absolute path where the file was saved.
    """
    session = await _get_session()
    try:
        saved_path = await session.save(path)
        return {"path": saved_path}
    except Exception as e:
        error_msg = str(e)
        is_write_error = "Read-only" in error_msg or "Failed to write" in error_msg
        if is_write_error and path is None:
            raise RuntimeError(
                "No path specified. For notebooks created with create_notebook(), "
                "you must provide a path (e.g., save_notebook('/path/to/file.ipynb'))"
            ) from e
        raise


def _format_notebook_list(rooms: list[dict[str, Any]]) -> str:
    """Format notebook rooms for terminal display."""
    if not rooms:
        return "No active notebooks"

    lines = [f"Notebooks ({len(rooms)}):"]
    for room in rooms:
        notebook_id = room.get("notebook_id", "unknown")
        peers = room.get("active_peers", 0)
        has_kernel = room.get("has_kernel", False)

        # Build kernel info
        if has_kernel:
            kernel_type = room.get("kernel_type", "unknown")
            kernel_status = room.get("kernel_status", "unknown")
            env_source = room.get("env_source", "unknown")
            kernel_info = f"{kernel_type} ({kernel_status}) | env: {env_source}"
        else:
            kernel_info = "no kernel"

        lines.append(f"\n• {notebook_id}")
        lines.append(f"  {kernel_info} | peers: {peers}")

    return "\n".join(lines)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def list_notebooks() -> list[ContentItem]:
    """List all active notebook rooms in the daemon.

    Returns:
        List of notebook rooms with their status.
    """
    client = _get_daemon_client()
    rooms = client.list_rooms()
    return [TextContent(type="text", text=_format_notebook_list([dict(room) for room in rooms]))]


# =============================================================================
# Kernel Management Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def start_kernel(
    kernel_type: str = "python",
    env_source: str = "auto",
) -> dict[str, Any]:
    """Start a kernel for the current session.

    If a kernel is already running for this notebook_id (from any client),
    it will be reused.

    Args:
        kernel_type: Type of kernel - "python" or "deno".
        env_source: Environment source. Options:
            - "auto" (default) - Auto-detect from notebook metadata/project files
            - "uv:prewarmed" - Fast startup from UV pool
            - "conda:prewarmed" - Conda environment from pool
            - "uv:inline" - Use notebook's inline UV dependencies
            - "conda:inline" - Use notebook's inline conda dependencies
            For Deno kernels, this is ignored (always uses "deno").

    Returns:
        Kernel info including the actual env_source used.
    """
    session = await _get_session()
    await session.start_kernel(kernel_type=kernel_type, env_source=env_source)

    return {
        "kernel_type": kernel_type,
        "env_source": await session.env_source(),
        "started": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def shutdown_kernel() -> dict[str, Any]:
    """Shutdown the kernel for the current session.

    Returns:
        Confirmation of shutdown.
    """
    session = await _get_session()
    await session.shutdown_kernel()

    return {"shutdown": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def interrupt_kernel() -> dict[str, Any]:
    """Interrupt the currently executing cell.

    Use this to stop long-running or infinite loops.

    Returns:
        Confirmation of interrupt.
    """
    session = await _get_session()
    await session.interrupt()

    return {"interrupted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def restart_kernel() -> dict[str, Any]:
    """Restart the kernel with updated dependencies.

    Clears all kernel state and reloads dependencies from notebook metadata.
    Use this after adding/removing dependencies when sync_environment()
    isn't sufficient.

    Returns:
        Confirmation of restart with the new env_source.
    """
    session = await _get_session()
    await session.restart_kernel(wait_for_ready=True)
    return {"restarted": True, "env_source": await session.env_source()}


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_kernel_status() -> dict[str, Any]:
    """Get the kernel status for the current session.

    Returns:
        Kernel status including whether it's running and the env_source.
    """
    session = await _get_session()

    return {
        "connected": await session.is_connected(),
        "kernel_started": await session.kernel_started(),
        "env_source": await session.env_source(),
    }


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def complete_code(code: str, cursor_pos: int) -> dict[str, Any]:
    """Get code completions from the kernel.

    Uses the kernel's introspection to provide context-aware completions.
    Requires a running kernel.

    Args:
        code: The code to complete.
        cursor_pos: Cursor position in the code (0-indexed byte offset).

    Returns:
        Completions including cursor_start, cursor_end, and items list.
    """
    session = await _get_session()
    result = await session.complete(code, cursor_pos)
    return {
        "cursor_start": result.cursor_start,
        "cursor_end": result.cursor_end,
        "items": [
            {"label": item.label, "kind": item.kind, "detail": item.detail} for item in result.items
        ],
    }


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_queue_state() -> dict[str, Any]:
    """Get the current execution queue state.

    Shows which cell is currently executing and which cells are queued.

    Returns:
        executing: Cell ID currently running (or null if idle).
        queued: List of cell IDs waiting to run.
    """
    session = await _get_session()
    state = await session.get_queue_state()
    return {"executing": state.executing, "queued": state.queued}


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_history(
    pattern: str | None = None,
    n: int = 100,
) -> dict[str, Any]:
    """Search kernel execution history.

    Returns previously executed code from this kernel session.

    Args:
        pattern: Optional glob pattern to filter results (e.g., "*import*").
        n: Maximum entries to return (default 100).

    Returns:
        entries: List of history entries with session, line, and source.
    """
    session = await _get_session()
    entries = await session.get_history(pattern=pattern, n=n, unique=True)
    return {
        "entries": [{"session": e.session, "line": e.line, "source": e.source} for e in entries]
    }


# =============================================================================
# Dependency Management Tools
# =============================================================================


async def _get_package_manager(session: runtimed.AsyncSession) -> str:
    """Detect which package manager the notebook is using.

    Detection order:
    1. If kernel is running, check env_source (most reliable)
    2. Otherwise, check stored metadata for existing dependencies
    3. Default to "uv" if no signal
    """
    # First check env_source if kernel is running
    env = await session.env_source()
    if env:
        if env.startswith("conda:"):
            return "conda"
        return "uv"

    # No kernel running - check stored metadata
    # If notebook has conda deps, it's a conda notebook
    conda_deps = await session.get_conda_dependencies()
    if conda_deps:
        return "conda"

    return "uv"


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def add_dependency(package: str) -> dict[str, Any]:
    """Add a Python package dependency to the notebook.

    Automatically uses the notebook's configured package manager (UV or Conda)
    based on env_source. The package is added to the notebook's inline
    dependency metadata.

    Use sync_environment() to install without restart, or restart_kernel()
    for a clean environment with the new dependency.

    Args:
        package: Package specifier (e.g., "pandas>=2.0", "requests").

    Returns:
        Updated list of dependencies and which package manager was used.
    """
    session = await _get_session()
    pm = await _get_package_manager(session)
    if pm == "conda":
        await session.add_conda_dependency(package)
        deps = await session.get_conda_dependencies()
    else:
        await session.add_uv_dependency(package)
        deps = await session.get_uv_dependencies()
    return {"dependencies": deps, "added": package, "package_manager": pm}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def remove_dependency(package: str) -> dict[str, Any]:
    """Remove a dependency from the notebook.

    Automatically uses the notebook's configured package manager (UV or Conda)
    based on env_source. Requires kernel restart to take effect.

    Args:
        package: Exact dependency string to remove.

    Returns:
        Updated list of dependencies and which package manager was used.
    """
    session = await _get_session()
    pm = await _get_package_manager(session)
    if pm == "conda":
        await session.remove_conda_dependency(package)
        deps = await session.get_conda_dependencies()
    else:
        await session.remove_uv_dependency(package)
        deps = await session.get_uv_dependencies()
    return {"dependencies": deps, "removed": package, "package_manager": pm}


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_dependencies() -> dict[str, Any]:
    """Get the notebook's current dependencies.

    Returns dependencies from the notebook's configured package manager
    (UV or Conda) based on env_source.

    Returns:
        List of dependency specifiers and which package manager is active.
    """
    session = await _get_session()
    pm = await _get_package_manager(session)
    if pm == "conda":
        deps = await session.get_conda_dependencies()
    else:
        deps = await session.get_uv_dependencies()
    return {"dependencies": deps, "package_manager": pm}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def sync_environment() -> dict[str, Any]:
    """Hot-install new dependencies without restarting the kernel.

    Only works for UV dependencies and only for additions.
    If removals are needed or sync fails, use restart_kernel() instead.

    Returns:
        success: Whether sync completed.
        synced_packages: Packages that were installed (if success).
        needs_restart: If true, must restart kernel instead.
    """
    session = await _get_session()
    result = await session.sync_environment()
    return {
        "success": result.success,
        "synced_packages": result.synced_packages,
        "error": result.error,
        "needs_restart": result.needs_restart,
    }


# =============================================================================
# Cell Operations Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def create_cell(
    source: str = "",
    cell_type: str = "code",
    index: int | None = None,
    and_run: bool = False,
    timeout_secs: float = 5.0,
) -> list[ContentItem]:
    """Create a new cell in the notebook, optionally executing it.

    The cell is added to the shared document and synced to all connected
    clients (including nteract if open with the same notebook).

    Args:
        source: Initial source code for the cell.
        cell_type: Cell type - "code", "markdown", or "raw".
        index: Position to insert the cell. None appends at the end.
        and_run: If True, execute the cell after creating it.
        timeout_secs: Max time to wait for execution (default 5s). If execution
            takes longer, returns partial results with complete=False.

    Returns:
        If and_run=False: The cell_id.
        If and_run=True: Cell with execution status and outputs.
    """
    session = await _get_session()
    cell_id = await session.create_cell(
        source=source,
        cell_type=cell_type,
        index=index,
    )

    if and_run and cell_type == "code":
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    return [TextContent(type="text", text=f"Created cell: {cell_id}")]


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def set_cell_source(cell_id: str, source: str) -> dict[str, Any]:
    """Update a cell's source code.

    The change is synced to all connected clients.

    Args:
        cell_id: The cell ID to update.
        source: The new source code.

    Returns:
        Confirmation of update.
    """
    session = await _get_session()
    await session.set_source(cell_id=cell_id, source=source)

    return {"cell_id": cell_id, "updated": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def append_source(cell_id: str, text: str) -> dict[str, Any]:
    """Append text to a cell's source code.

    Uses direct CRDT insertion (no diff) - ideal for streaming LLM tokens.
    Changes sync to all connected clients in real-time.

    Args:
        cell_id: The cell ID to append to.
        text: The text to append.

    Returns:
        Confirmation of append.
    """
    session = await _get_session()
    await session.append_source(cell_id=cell_id, text=text)

    return {"cell_id": cell_id, "appended": True}


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_cell(cell_id: str) -> list[ContentItem]:
    """Get a cell by ID, including outputs if available.

    Outputs are resolved from the Automerge document, so you can see
    outputs from cells executed by other clients.

    Args:
        cell_id: The cell ID.

    Returns:
        Cell with source code and outputs (images returned as ImageContent).
    """
    session = await _get_session()
    cell = await session.get_cell(cell_id=cell_id)
    return _cell_to_content(cell)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_all_cells() -> list[ContentItem]:
    """Get all cells in the current notebook, including outputs.

    Outputs are resolved from the Automerge document, so you can see
    outputs from cells executed by other clients.

    Returns:
        All cells with source code and outputs (images returned as ImageContent).
    """
    session = await _get_session()
    cells = await session.get_cells()
    items: list[ContentItem] = []
    for cell in cells:
        items.extend(_cell_to_content(cell))
    return items


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def delete_cell(cell_id: str) -> dict[str, Any]:
    """Delete a cell from the notebook.

    The change is synced to all connected clients.

    Args:
        cell_id: The cell ID to delete.

    Returns:
        Confirmation of deletion.
    """
    session = await _get_session()
    await session.delete_cell(cell_id=cell_id)

    return {"cell_id": cell_id, "deleted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def move_cell(cell_id: str, after_cell_id: str | None = None) -> dict[str, Any]:
    """Move a cell to a new position in the notebook.

    Reorders the shared document and syncs the change to all connected clients.

    Args:
        cell_id: The cell to move.
        after_cell_id: Move after this cell. Use None to move to the start.

    Returns:
        Confirmation of the move, including the new internal position token.
    """
    session = await _get_session()
    new_position = await session.move_cell(cell_id=cell_id, after_cell_id=after_cell_id)
    return {
        "cell_id": cell_id,
        "after_cell_id": after_cell_id,
        "new_position": new_position,
        "moved": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def clear_outputs(cell_id: str) -> dict[str, Any]:
    """Clear a cell's outputs.

    Removes all outputs from the cell. Useful before re-running a cell
    to get a clean slate.

    Args:
        cell_id: The cell ID to clear outputs from.

    Returns:
        Confirmation of clearing.
    """
    session = await _get_session()
    await session.clear_outputs(cell_id)
    return {"cell_id": cell_id, "cleared": True}


# =============================================================================
# Execution Tools
# =============================================================================


async def _execute_cell_internal(
    cell_id: str,
    timeout_secs: float = 5.0,
) -> list[ContentItem]:
    """Internal execution with streaming and partial results."""
    session = await _get_session()
    events: list[Any] = []  # list[runtimed.ExecutionEvent]
    complete = False

    async def collect_events() -> None:
        nonlocal complete
        async for event in await session.stream_execute(cell_id):
            events.append(event)
            if event.event_type in ("done", "error"):
                complete = True
                break

    with contextlib.suppress(asyncio.TimeoutError):
        await asyncio.wait_for(collect_events(), timeout=timeout_secs)

    if complete:
        # Prefer the synced document as the final source of truth once execution
        # finishes. This is more robust across runtimed output transport changes.
        session = await _get_session()
        with contextlib.suppress(Exception):
            cell = await session.get_cell(cell_id=cell_id)
            has_error_output = any(output.output_type == "error" for output in cell.outputs)
            status = "error" if has_error_output else "idle"
            header = _format_header(cell.id, status=status, execution_count=cell.execution_count)
            items: list[ContentItem] = [TextContent(type="text", text=header)]
            items.extend(_outputs_to_content(cell.outputs))
            return items

    return _execution_result_to_content(cell_id, events, complete)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def execute_cell(
    cell_id: str,
    timeout_secs: float = 5.0,
) -> list[ContentItem]:
    """Execute a cell by ID.

    Returns partial results after timeout_secs if still running.
    If status shows "running", use get_cell() to poll for more outputs.

    Args:
        cell_id: The cell ID to execute.
        timeout_secs: Maximum time to wait for execution (default: 5s).

    Returns:
        Cell with execution status and outputs (images returned as ImageContent).
    """
    return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def run_all_cells() -> dict[str, Any]:
    """Queue all code cells for execution.

    Queues all code cells in document order. Does not wait for completion.
    Use get_queue_state() to monitor progress or get_all_cells() to see results.

    Returns:
        count: Number of cells queued for execution.
    """
    session = await _get_session()
    count = await session.run_all_cells()
    return {"status": "queued", "count": count}


# =============================================================================
# Resources
# =============================================================================


@mcp.resource("notebook://cells")
async def resource_cells() -> str:
    """Get all cells in the current notebook."""
    if _session is None:
        return "Error: No active session"

    try:
        cells = await _session.get_cells()
        formatted = [
            _format_cell(
                cell,
            )
            for cell in cells
        ]
        return "\n\n".join(formatted)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://status")
async def resource_status() -> str:
    """Get the current session and kernel status as JSON."""
    if _session is None:
        return json.dumps(
            {
                "connected": False,
                "kernel_started": False,
                "env_source": None,
            }
        )

    try:
        return json.dumps(
            {
                "notebook_id": _session.notebook_id,
                "connected": await _session.is_connected(),
                "kernel_started": await _session.kernel_started(),
                "env_source": await _session.env_source(),
            }
        )
    except Exception as e:
        return json.dumps({"error": str(e)})


@mcp.resource("notebook://rooms")
async def resource_rooms() -> str:
    """Get all active notebook rooms as JSON."""
    try:
        client = _get_daemon_client()
        rooms = client.list_rooms()
        return json.dumps([dict(room) for room in rooms])
    except Exception as e:
        return json.dumps({"error": str(e)})


# =============================================================================
# Entry Point
# =============================================================================


def main():
    """Run the MCP server."""
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        stream=sys.stderr,
    )
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
