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
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Connect to a notebook by ID. Omit notebook_id to create a new session."""
    global _session
    if ctx:
        _sniff_client_name(ctx)

    # Close existing session if any
    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    # Create new session
    _session = runtimed.AsyncSession(notebook_id=notebook_id, peer_label=_peer_label())
    await _session.connect()

    return {
        "notebook_id": _session.notebook_id,
        "connected": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def open_notebook(path: str, ctx: Context | None = None) -> dict[str, Any]:
    """Open an existing .ipynb file. Use create_notebook() for new notebooks."""
    global _session
    if ctx:
        _sniff_client_name(ctx)

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.open_notebook(path, peer_label=_peer_label())
    info = await _session.connection_info()
    return {
        "notebook_id": _session.notebook_id,
        "path": path,
        "cell_count": info.cell_count if info else 0,
        "needs_trust_approval": info.needs_trust_approval if info else False,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def create_notebook(
    runtime: Literal["python", "deno"] = "python",
    working_dir: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Create a new empty notebook in memory. Call save_notebook(path) to persist to disk."""
    global _session
    if ctx:
        _sniff_client_name(ctx)

    if _session is not None:
        with contextlib.suppress(Exception):
            await _session.close()

    _session = await runtimed.AsyncSession.create_notebook(
        runtime=runtime, working_dir=working_dir, peer_label=_peer_label()
    )
    info = await _session.connection_info()
    return {
        "notebook_id": _session.notebook_id,
        "runtime": runtime,
        "cell_count": info.cell_count if info else 1,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def save_notebook(path: str | None = None) -> dict[str, Any]:
    """Save notebook to disk. Path is required for create_notebook() notebooks."""
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
async def restart_kernel() -> dict[str, Any]:
    """Restart kernel, clearing all state. Use after dependency changes."""
    session = await _get_session()
    await session.restart_kernel(wait_for_ready=True)
    return {"restarted": True, "env_source": await session.env_source()}


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
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 5.0,
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
async def set_cell_source(
    cell_id: str,
    source: Annotated[str, Field(description="Complete new source code for the cell")],
    and_run: Annotated[bool, Field(description="Execute the cell immediately after edit")] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 5.0,
) -> ContentItem | list[ContentItem]:
    """Replace a cell's entire source. Prefer replace_match for targeted edits."""
    session = await _get_session()
    await session.set_source(cell_id=cell_id, source=source)

    if and_run:
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
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 5.0,
) -> ContentItem | list[ContentItem]:
    """Replace matched text in a cell. Prefer this for simple, targeted edits.

    Use context_before/context_after to disambiguate when match appears multiple times.
    Fails if 0 or >1 matches (reports count + offsets). Use replace_regex for
    zero-width insertions or structural patterns.
    """
    from nteract._editing import PatternError
    from nteract._editing import replace_match as _replace_match

    session = await _get_session()
    cell = await session.get_cell(cell_id=cell_id)
    source = cell.source

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
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 5.0,
) -> ContentItem | list[ContentItem]:
    """Replace a regex-matched span. Use for anchors, lookarounds, or zero-width insertions.

    Fails if 0 or >1 matches (reports count + offsets for disambiguation).
    """
    from nteract._editing import PatternError
    from nteract._editing import replace_regex as _replace_regex

    session = await _get_session()
    cell = await session.get_cell(cell_id=cell_id)
    source = cell.source

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


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def append_source(
    cell_id: str,
    text: Annotated[str, Field(description="Text to append to the cell source")],
    and_run: Annotated[bool, Field(description="Execute the cell immediately after edit")] = False,
    timeout_secs: Annotated[float, Field(description="Max seconds to wait for execution")] = 5.0,
) -> ContentItem | list[ContentItem]:
    """Append text to a cell's source. Ideal for streaming tokens."""
    session = await _get_session()
    await session.append_source(cell_id=cell_id, text=text)

    if and_run:
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    return TextContent(type="text", text=f'Appended to cell "{cell_id}"')


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_cell(
    cell_id: str,
) -> list[ContentItem]:
    """Get a cell's source and outputs by ID."""
    session = await _get_session()
    cell = await session.get_cell(cell_id=cell_id)
    return _cell_to_content(cell)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_all_cells() -> list[ContentItem]:
    """Get all cells with source and outputs."""
    session = await _get_session()
    cells = await session.get_cells()
    items: list[ContentItem] = []
    for cell in cells:
        items.extend(_cell_to_content(cell))
    return items


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
    timeout_secs: Annotated[
        float, Field(description="Max seconds to wait; returns partial results if exceeded")
    ] = 5.0,
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
