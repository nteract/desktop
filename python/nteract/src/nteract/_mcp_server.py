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
import difflib
import json
import logging
import os
import re
import sys
from typing import Annotated, Any, Literal

from mcp.server.fastmcp import Context, FastMCP
from mcp.types import ImageContent, TextContent, ToolAnnotations
from pydantic import Field

import runtimed

logger = logging.getLogger(__name__)

# MCP content types for tool responses
ContentItem = TextContent | ImageContent

# Create the MCP server
mcp = FastMCP("nteract")

# When --no-show is passed, the show_notebook tool is not registered.
# This is useful for headless environments where no desktop app is available.
_no_show = "--no-show" in sys.argv


# ── Peer label for remote cursors ─────────────────────────────────────
# The MCP initialize handshake includes clientInfo with `name` and
# `title` fields. We prefer `title` ("Codex") over `name`
# ("codex-mcp-client") for the cursor flag.

_client_name: str | None = None


def _sniff_client_name(ctx: Context) -> None:
    """Extract clientInfo from the MCP session. Lazy, first call only."""
    global _client_name
    if _client_name is not None:
        return
    try:
        params = ctx.request_context.session.client_params
        if params and params.clientInfo:
            info = params.clientInfo
            _client_name = getattr(info, "title", None) or info.name
    except Exception:
        pass


def _peer_label() -> str:
    """Return a label for the cursor flag. Falls back to 'Agent'."""
    return _client_name or "Agent"


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
        raise RuntimeError("No active notebook session. Call join_notebook first.")
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


def _build_cell_status_map(queue_state: runtimed.QueueState) -> dict[str, str]:
    """Build a cell_id -> status mapping from queue state."""
    cell_status: dict[str, str] = {}
    if queue_state.executing:
        cell_status[queue_state.executing] = "running"
    for cid in queue_state.queued:
        cell_status[cid] = "queued"
    return cell_status


async def _get_cell_status_map(session: runtimed.AsyncSession) -> dict[str, str]:
    """Fetch queue state and return cell status map, empty on failure.

    Status is a best-effort annotation — errors should never prevent
    get_all_cells or get_cell from returning results.
    """
    try:
        queue_state = await session.get_queue_state()
        return _build_cell_status_map(queue_state)
    except asyncio.CancelledError:
        raise
    except Exception:
        return {}


async def _get_single_cell_status(session: runtimed.AsyncSession, cell_id: str) -> str | None:
    """Fetch queue status for a single cell, None on failure."""
    try:
        queue_state = await session.get_queue_state()
        if queue_state.executing == cell_id:
            return "running"
        if cell_id in queue_state.queued:
            return "queued"
        return None
    except asyncio.CancelledError:
        raise
    except Exception:
        return None


def _format_cell_summary(
    index: int,
    cell: runtimed.Cell,
    preview_chars: int = 60,
    include_outputs: bool = False,
    status: str | None = None,
) -> str:
    """Format a cell as a single summary line.

    Example output:
    0 | markdown | id=cell-1be2a179-... | # Crate Download Analysis
    1 | code | running | id=cell-e18fcc2a-... | exec=4 | import requests…[+45 chars]
    2 | code | queued | id=cell-abc123-... | exec=3 | df.plot()…[+20 chars]
    """
    parts = [str(index), cell.cell_type]

    if status:
        parts.append(status)

    parts.append(f"id={cell.id}")

    if cell.cell_type == "code" and cell.execution_count is not None:
        parts.append(f"exec={cell.execution_count}")

    # Source preview
    if cell.source:
        # Collapse to single line, strip leading/trailing whitespace
        source_line = " ".join(cell.source.split())
        if len(source_line) > preview_chars:
            remaining = len(source_line) - preview_chars
            source_line = f"{source_line[:preview_chars]}…[+{remaining} chars]"
        parts.append(source_line)

    line = " | ".join(parts)

    # Optional output preview
    if include_outputs and cell.outputs:
        output_text = _format_outputs_text(cell.outputs)
        if output_text:
            # Collapse to single line
            output_line = " ".join(output_text.split())
            if len(output_line) > preview_chars:
                remaining = len(output_line) - preview_chars
                output_line = f"{output_line[:preview_chars]}…[+{remaining} chars]"
            line += f"\n  └─ {output_line}"

    return line


def _format_header(
    cell_id: str,
    cell_type: str | None = None,
    status: str | None = None,
    execution_count: int | None = None,
) -> str:
    """Format a cell header line for terminal display.

    Example: ━━━ cell-abc12345 (code) ✓ idle [3] ━━━
    """
    icons = {"idle": "✓", "error": "✗", "running": "◐", "queued": "⧗"}

    parts = [f"━━━ {cell_id}"]

    if cell_type:
        parts.append(f"({cell_type})")

    if status:
        icon = icons.get(status, "?")
        parts.append(f"{icon} {status}")

    if execution_count is not None:
        parts.append(f"[{execution_count}]")

    parts.append("━━━")
    return " ".join(parts)


def _format_cell(cell: runtimed.Cell, status: str | None = None) -> str:
    """Format a cell for terminal display (includes source).

    Used by get_cell to show full cell state.
    """
    header = _format_header(
        cell.id, cell_type=cell.cell_type, status=status, execution_count=cell.execution_count
    )
    output_text = _format_outputs_text(cell.outputs)

    if cell.source and output_text:
        return f"{header}\n\n{cell.source}\n\n───────────────────\n\n{output_text}"
    elif cell.source:
        return f"{header}\n\n{cell.source}"
    elif output_text:
        return f"{header}\n\n{output_text}"
    else:
        return header


def _cell_to_content(cell: runtimed.Cell, status: str | None = None) -> list[ContentItem]:
    """Convert a cell to rich MCP content items.

    Returns a header as TextContent, then each output as its richest type.
    """
    header = _format_header(
        cell.id, cell_type=cell.cell_type, status=status, execution_count=cell.execution_count
    )
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


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def list_active_notebooks() -> list[dict[str, Any]]:
    """List all open notebook sessions.

    Returns notebooks currently open by users or other agents.
    Use join_notebook(notebook_id) to connect to one.
    """
    client = _get_daemon_client()
    rooms = client.list_rooms()
    return [
        {
            "notebook_id": room["notebook_id"],
            "active_peers": room["active_peers"],
            "has_kernel": room["has_kernel"],
            "kernel_type": room.get("kernel_type"),
            "kernel_status": room.get("kernel_status"),
        }
        for room in rooms
    ]


if not _no_show:

    @mcp.tool()
    async def show_notebook(
        notebook_id: Annotated[
            str | None,
            Field(
                description="Notebook ID to show. Defaults to current session's notebook.",
            ),
        ] = None,
    ) -> str:
        """Open the notebook in the nteract desktop app.

        The notebook must be currently running in the daemon. If no notebook_id
        is provided, opens the notebook from the current session.
        """
        target = notebook_id
        if target is None:
            if _session is not None:
                target = _session.notebook_id
            else:
                raise ValueError(
                    "No notebook_id provided and no active session. "
                    "Use list_active_notebooks() to find a notebook_id, or connect to one first."
                )

        client = _get_daemon_client()
        rooms = client.list_rooms()
        room_ids = {room["notebook_id"] for room in rooms}
        if target not in room_ids:
            raise ValueError(
                f"Notebook '{target}' is not currently running. "
                f"Use list_active_notebooks() to see active notebooks."
            )

        if not os.path.isabs(target):
            raise ValueError(
                f"Notebook '{target}' is an untitled notebook (not saved to disk). "
                f"Use save_notebook(path) first, then call show_notebook()."
            )

        runtimed.show_notebook_app(target)
        return f"Opened notebook in nteract: {target}"


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def join_notebook(
    notebook_id: str,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Join an existing notebook session by ID.

    Use list_active_notebooks() to see available sessions. To open a file from disk,
    use open_notebook(path). To create a new notebook, use create_notebook().
    """
    global _session
    if ctx:
        _sniff_client_name(ctx)

    # Close existing session if any
    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    # Join existing session
    _session = runtimed.AsyncSession(notebook_id=notebook_id, peer_label=_peer_label())
    session = _session
    await session.connect()

    return {
        "notebook_id": session.notebook_id,
        "connected": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def open_notebook(path: str, ctx: Context | None = None) -> dict[str, Any]:
    """Open an existing .ipynb file. The kernel starts automatically.

    Use create_notebook() for new notebooks.
    """
    global _session
    if ctx:
        _sniff_client_name(ctx)

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.open_notebook(path, peer_label=_peer_label())
    session = _session
    return {
        "notebook_id": session.notebook_id,
        "path": path,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def create_notebook(
    runtime: Literal["python", "deno"] = "python",
    working_dir: Annotated[
        str,
        Field(
            description="Working directory for the kernel and"
            " environment detection (e.g. pyproject.toml)."
            f" Defaults to {os.getcwd()}"
        ),
    ] = os.getcwd(),
    dependencies: Annotated[
        list[str] | None,
        Field(
            description="Python packages to pre-install (e.g. ['pandas', 'requests'])."
            " Set before kernel launch so the first start includes them."
        ),
    ] = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Create a new notebook with optional pre-installed dependencies.

    The kernel starts automatically. If dependencies are provided, they are
    added to notebook metadata before the kernel launches, so the environment
    is prepared on first start (no restart needed).

    Call save_notebook(path) to persist to disk.
    """
    global _session
    if ctx:
        _sniff_client_name(ctx)

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.create_notebook(
        runtime=runtime, working_dir=working_dir, peer_label=_peer_label()
    )
    session = _session

    if dependencies and runtime == "python":
        # Add dependencies to notebook metadata
        for dep in dependencies:
            await session.add_uv_dependency(dep)

        # The daemon may have auto-launched a kernel (without these deps).
        # Restart to ensure the kernel picks up the inline deps.
        with contextlib.suppress(Exception):
            await session.restart_kernel(wait_for_ready=True)

    return {
        "notebook_id": session.notebook_id,
        "runtime": runtime,
        "dependencies": dependencies or [],
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def save_notebook(path: str | None = None) -> dict[str, Any]:
    """Save notebook to disk.

    The daemon automatically re-keys ephemeral (UUID-based) rooms to the saved
    file path, so no disconnect/reconnect is needed.
    """
    session = await _get_session()

    try:
        saved_path = await session.save(path)
    except Exception as e:
        error_msg = str(e)
        is_write_error = "Read-only" in error_msg or "Failed to write" in error_msg
        if is_write_error and path is None:
            raise RuntimeError(
                "No path specified. For notebooks created with create_notebook(), "
                "you must provide a path (e.g., save_notebook('/path/to/file.ipynb'))"
            ) from e
        raise

    return {"path": saved_path, "notebook_id": session.notebook_id}


# =============================================================================
# Kernel Management Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def interrupt_kernel() -> dict[str, Any]:
    """Interrupt the currently executing cell."""
    session = await _get_session()
    await session.interrupt()

    return {"interrupted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def restart_kernel(ctx: Context | None = None) -> dict[str, Any]:
    """Restart kernel, clearing all state. Use after dependency changes.

    Reports environment preparation progress (package downloads, installs)
    via MCP log notifications while waiting. Times out after 120s.
    """
    session = await _get_session()
    progress_messages: list[str] = await session.restart_kernel(wait_for_ready=True)

    # Send progress messages as MCP log notifications (retroactively)
    if ctx and progress_messages:
        for msg in progress_messages:
            with contextlib.suppress(Exception):
                await ctx.info(msg)

    return {
        "restarted": True,
        "env_source": await session.env_source(),
        "progress": progress_messages,
    }


# =============================================================================
# Dependency Management Tools
# =============================================================================


async def _get_package_manager(session: runtimed.AsyncSession) -> str:
    """Detect which package manager the notebook is using.

    Detection order:
    1. If kernel is running, check env_source (most reliable)
    2. Check notebook metadata structure (conda vs uv)
    3. Check session's settings replica for default_python_env
    4. Default to "uv" if no signal
    """
    # First check env_source if kernel is running
    env = await session.env_source()
    if env:
        if env.startswith("conda:"):
            return "conda"
        return "uv"

    # Check metadata structure (not just non-empty deps)
    env_type = await session.get_metadata_env_type()
    if env_type:
        return env_type

    # Check settings from session's local replica (no round-trip)
    settings = session.get_settings()
    if settings:
        return settings.get("default_python_env", "uv")

    return "uv"


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def add_dependency(package: str) -> dict[str, Any]:
    """Add a package dependency (e.g. "pandas>=2.0"). Call sync_environment() to install."""
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
    """Remove a package dependency. Requires restart_kernel() to take effect."""
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
    """Get the notebook's current package dependencies."""
    session = await _get_session()
    pm = await _get_package_manager(session)
    if pm == "conda":
        deps = await session.get_conda_dependencies()
    else:
        deps = await session.get_uv_dependencies()
    return {"dependencies": deps, "package_manager": pm}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def sync_environment() -> dict[str, Any]:
    """Hot-install new dependencies without restarting. Use restart_kernel() if this fails."""
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
    cell_type: Literal["code", "markdown", "raw"] = "code",
    index: Annotated[
        int | None, Field(description="Position to insert. None appends at end")
    ] = None,
    and_run: Annotated[
        bool, Field(description="Execute the cell immediately after creation")
    ] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 30.0,
) -> list[ContentItem]:
    """Create a cell, optionally executing it."""
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
async def set_cell(
    cell_id: str,
    source: Annotated[
        str | None, Field(description="New source code (None to leave unchanged)")
    ] = None,
    cell_type: Annotated[
        Literal["code", "markdown", "raw"] | None,
        Field(description="New cell type (None to leave unchanged)"),
    ] = None,
    and_run: Annotated[
        bool, Field(description="Execute the cell after changes (code cells only)")
    ] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 30.0,
) -> ContentItem | list[ContentItem]:
    """Update a cell's source and/or type. Use replace_match for targeted edits."""
    session = await _get_session()

    # Update source if provided
    if source is not None:
        await session.set_source(cell_id=cell_id, source=source)

    # Update cell type if provided
    if cell_type is not None:
        await session.set_cell_type(cell_id=cell_id, cell_type=cell_type)

    # If nothing was changed, return current cell state
    if source is None and cell_type is None:
        return TextContent(type="text", text=f'Cell "{cell_id}" unchanged (no updates specified)')

    if and_run:
        ct = await session.get_cell_type(cell_id=cell_id)
        if ct is None:
            return TextContent(type="text", text=f'Cell "{cell_id}" not found')
        if ct == "code":
            return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    return TextContent(type="text", text=f'Cell "{cell_id}" updated')


async def _send_edit_cursor(
    session: runtimed.AsyncSession, cell_id: str, source: str, offset: int
) -> None:
    """Send cursor presence at a character offset (best-effort, non-blocking)."""
    from nteract._editing import offset_to_line_col

    try:
        line, col = offset_to_line_col(source, offset)
        await session.set_cursor(cell_id=cell_id, line=line, column=col)
    except Exception:
        pass  # Presence is best-effort — don't fail the edit


async def _send_cell_cursor(
    session: runtimed.AsyncSession, cell_id: str, line: int = 0, column: int = 0
) -> None:
    """Send cursor presence on a cell (best-effort, errors silently ignored)."""
    with contextlib.suppress(Exception):
        await session.set_cursor(cell_id=cell_id, line=line, column=column)


def _format_edit_diff(cell_id: str, old_text: str, new_text: str) -> str:
    """Format a unified diff for an edit operation."""
    # splitlines(keepends=False) + manual newlines ensures consistent output
    old_lines = [line + "\n" for line in old_text.splitlines()]
    new_lines = [line + "\n" for line in new_text.splitlines()]
    diff = difflib.unified_diff(old_lines, new_lines, fromfile="before", tofile="after")
    diff_text = "".join(diff)
    return f'Edited cell "{cell_id}":\n{diff_text}'


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def replace_match(
    cell_id: str,
    match: Annotated[str, Field(description="Literal text to find (must match exactly once)")],
    content: Annotated[
        str, Field(description="Literal replacement text — real newlines, no escapes")
    ],
    context_before: Annotated[
        str, Field(description="Text that must appear before the match")
    ] = "",
    context_after: Annotated[str, Field(description="Text that must appear after the match")] = "",
    and_run: Annotated[bool, Field(description="Execute the cell immediately after edit")] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 30.0,
) -> ContentItem | list[ContentItem]:
    """Replace matched text in a cell. Prefer this for simple, targeted edits.

    Use context_before/context_after to disambiguate when match appears multiple times.
    Fails if 0 or >1 matches (reports count + offsets). Use replace_regex for
    zero-width insertions or structural patterns.
    """
    from nteract._editing import PatternError
    from nteract._editing import replace_match as _replace_match

    session = await _get_session()
    source = await session.get_cell_source(cell_id=cell_id)
    if source is None:
        return TextContent(type="text", text=f'Cell "{cell_id}" not found')

    try:
        result = _replace_match(source, match, content, context_before, context_after)
    except PatternError as e:
        raise RuntimeError(f"{e} (match_count={e.match_count}, source_length={len(source)})") from e

    # Show cursor at edit location before applying
    await _send_edit_cursor(session, cell_id, source, result.span.start)

    await session.set_source(cell_id=cell_id, source=result.new_source)

    # Move cursor to end of replacement
    end_offset = result.span.start + len(content)
    await _send_edit_cursor(session, cell_id, result.new_source, end_offset)

    if and_run:
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    diff = _format_edit_diff(cell_id, result.old_text, content)
    return TextContent(type="text", text=diff)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def replace_regex(
    cell_id: str,
    pattern: Annotated[
        str,
        Field(
            description="Python regex, must match exactly once. "
            "re.MULTILINE enabled. Use \\Z for end-of-cell, (?=...) for insertions"
        ),
    ],
    content: Annotated[
        str, Field(description="Literal replacement text — not re.sub syntax, no backreferences")
    ],
    and_run: Annotated[bool, Field(description="Execute the cell immediately after edit")] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 30.0,
) -> ContentItem | list[ContentItem]:
    """Replace a regex-matched span. Use for anchors, lookarounds, or zero-width insertions.

    Fails if 0 or >1 matches (reports count + offsets for disambiguation).
    """
    from nteract._editing import PatternError
    from nteract._editing import replace_regex as _replace_regex

    session = await _get_session()
    source = await session.get_cell_source(cell_id=cell_id)
    if source is None:
        return TextContent(type="text", text=f'Cell "{cell_id}" not found')

    try:
        result = _replace_regex(source, pattern, content)
    except PatternError as e:
        raise RuntimeError(f"{e} (match_count={e.match_count}, source_length={len(source)})") from e

    # Show cursor at edit location before applying
    await _send_edit_cursor(session, cell_id, source, result.span.start)

    await session.set_source(cell_id=cell_id, source=result.new_source)

    # Move cursor to end of replacement
    end_offset = result.span.start + len(content)
    await _send_edit_cursor(session, cell_id, result.new_source, end_offset)

    if and_run:
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    diff = _format_edit_diff(cell_id, result.old_text, content)
    return TextContent(type="text", text=diff)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_cell(
    cell_id: str,
) -> list[ContentItem]:
    """Get a cell's source and outputs by ID."""
    session = await _get_session()
    await _send_cell_cursor(session, cell_id)
    cell = await session.get_cell(cell_id=cell_id)
    if cell is None:
        return [TextContent(type="text", text=f'Cell "{cell_id}" not found')]
    status = await _get_single_cell_status(session, cell_id)
    return _cell_to_content(cell, status=status)


def _output_to_dict(output: runtimed.Output) -> dict[str, Any]:
    """Convert an output to a dictionary for JSON serialization."""
    result: dict[str, Any] = {"output_type": output.output_type}

    if output.output_type == "stream":
        result["name"] = output.name
        result["text"] = _strip_ansi(output.text) if output.text else ""
    elif output.output_type == "error":
        result["ename"] = output.ename or ""
        result["evalue"] = output.evalue or ""
        result["traceback"] = output.traceback or []
    elif output.output_type in ("display_data", "execute_result"):
        # Include text-based data, skip large binary data
        result["data"] = {}
        if output.data:
            for mime in _TEXT_MIME_PRIORITY:
                if mime in output.data:
                    result["data"][mime] = output.data[mime]
                    break
        if output.output_type == "execute_result" and output.execution_count is not None:
            result["execution_count"] = output.execution_count

    return result


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_all_cells(
    format: Annotated[
        Literal["summary", "json", "rich"],
        Field(description="'summary' (default), 'json', or 'rich' for full content"),
    ] = "summary",
    start: Annotated[int, Field(description="Starting cell index (0-based)")] = 0,
    count: Annotated[
        int | None, Field(description="Number of cells to return (None = all)")
    ] = None,
    include_outputs: Annotated[
        bool, Field(description="Include output previews in summary format")
    ] = False,
    preview_chars: Annotated[int, Field(description="Max chars for source preview")] = 60,
) -> str | list[ContentItem] | list[dict[str, Any]]:
    """Get all cells. Use summary (default) for discovery, get_cell() for details.

    Args:
        format: "summary" for compact overview, "json" for structured data,
            "rich" for full content with images.
        start: Starting cell index for pagination.
        count: Number of cells to return (None = all remaining).
        include_outputs: Include output previews in summary format.
        preview_chars: Max characters for source/output preview in summary format.
    """
    # Validate pagination params
    if start < 0:
        raise ValueError(f"start must be >= 0, got {start}")
    if count is not None and count < 0:
        raise ValueError(f"count must be >= 0 or None, got {count}")
    if preview_chars < 1:
        raise ValueError(f"preview_chars must be >= 1, got {preview_chars}")

    session = await _get_session()
    cells = await session.get_cells()

    # Fetch execution queue state to annotate running/queued cells
    cell_status = await _get_cell_status_map(session)

    # Apply pagination
    end = start + count if count is not None else len(cells)
    cells = cells[start:end]

    if format == "json":
        return [
            {
                "cell_id": cell.id,
                "cell_type": cell.cell_type,
                "execution_count": cell.execution_count,
                "source": cell.source,
                "outputs": [_output_to_dict(o) for o in cell.outputs],
                "status": cell_status.get(cell.id),
            }
            for cell in cells
        ]

    if format == "rich":
        items: list[ContentItem] = []
        for cell in cells:
            items.extend(_cell_to_content(cell, status=cell_status.get(cell.id)))
        return items

    # Default summary format - compact one-line-per-cell
    lines = [
        _format_cell_summary(
            start + i,
            cell,
            preview_chars,
            include_outputs,
            status=cell_status.get(cell.id),
        )
        for i, cell in enumerate(cells)
    ]
    return "\n".join(lines)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def delete_cell(cell_id: str) -> dict[str, Any]:
    """Delete a cell by ID."""
    session = await _get_session()
    await session.delete_cell(cell_id=cell_id)

    return {"cell_id": cell_id, "deleted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def move_cell(
    cell_id: str,
    after_cell_id: Annotated[
        str | None, Field(description="Move after this cell, or null for start")
    ] = None,
) -> dict[str, Any]:
    """Move a cell to a new position."""
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
    """Clear a cell's outputs."""
    session = await _get_session()
    await session.clear_outputs(cell_id)
    return {"cell_id": cell_id, "cleared": True}


# =============================================================================
# Execution Tools
# =============================================================================


async def _execute_cell_internal(
    cell_id: str,
    timeout_secs: float = 30.0,
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
            if cell is None:
                raise ValueError(f'Cell "{cell_id}" not found')
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
    timeout_secs: Annotated[
        float, Field(description="Max seconds to wait; returns partial results if exceeded")
    ] = 30.0,
) -> list[ContentItem]:
    """Execute a cell. Returns partial results if timeout exceeded."""
    return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def run_all_cells() -> dict[str, Any]:
    """Queue all code cells for execution. Use get_all_cells() to see results."""
    session = await _get_session()
    count = await session.run_all_cells()
    return {"status": "queued", "count": count}


# =============================================================================
# Resources
# =============================================================================


@mcp.resource("notebook://cells")
async def resource_cells() -> str:
    """Get all cells in the current notebook as a compact summary."""
    if _session is None:
        return "Error: No active session"

    try:
        cells = await _session.get_cells()
        cell_status = await _get_cell_status_map(_session)
        lines = [
            _format_cell_summary(i, cell, status=cell_status.get(cell.id))
            for i, cell in enumerate(cells)
        ]
        return "\n".join(lines)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cell/{cell_id}")
async def resource_cell(cell_id: str) -> str:
    """Get a specific cell's source and outputs."""
    if _session is None:
        return "Error: No active session"

    try:
        await _send_cell_cursor(_session, cell_id)
        cell = await _session.get_cell(cell_id=cell_id)
        if cell is None:
            return f"Error: Cell {cell_id} not found"
        status = await _get_single_cell_status(_session, cell_id)
        return _format_cell(cell, status=status)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cells/by-index/{index}")
async def resource_cell_by_index(index: int) -> str:
    """Get the cell at the specified position (0-based index)."""
    if _session is None:
        return "Error: No active session"

    try:
        cell_ids = await _session.get_cell_ids()
        if index < 0 or index >= len(cell_ids):
            return f"Error: Index {index} out of range (notebook has {len(cell_ids)} cells)"
        cell_id = cell_ids[index]
        await _send_cell_cursor(_session, cell_id)
        cell = await _session.get_cell(cell_id=cell_id)
        if cell is None:
            return f"Error: Cell at index {index} not found"
        status = await _get_single_cell_status(_session, cell_id)
        return _format_cell(cell, status=status)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cell/{cell_id}/outputs")
async def resource_cell_outputs(cell_id: str) -> str:
    """Get a specific cell's outputs only (text format)."""
    if _session is None:
        return "Error: No active session"

    try:
        await _send_cell_cursor(_session, cell_id)
        cell = await _session.get_cell(cell_id=cell_id)
        if cell is None:
            return f"Error: Cell {cell_id} not found"
        output_text = _format_outputs_text(cell.outputs)
        if not output_text:
            return "(no outputs)"
        return output_text
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
    try:
        mcp.run(transport="stdio")
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
