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
import base64
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
from runtimed.runtimed import QueueState

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


# Notebook state - single active notebook at a time
_notebook: runtimed.Notebook | None = None
_client: runtimed.Client | None = None


def _get_client() -> runtimed.Client:
    """Get or create the client."""
    global _client
    if _client is None:
        _client = runtimed.Client()
    return _client


async def _get_notebook() -> runtimed.Notebook:
    """Get the current notebook, raising error if not connected."""
    if _notebook is None:
        raise RuntimeError("No active notebook session. Call join_notebook first.")
    return _notebook


# Regex to strip ANSI escape sequences (terminal colors, cursor movement, etc.)
_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B")


def _strip_ansi(text: str) -> str:
    """Strip ANSI escape sequences from text.

    Kernel stream output (especially from pip/uv installs) often contains
    terminal control codes for colors, progress bars, and cursor movement.
    These waste LLM context and render as garbage in text responses.
    """
    return _ANSI_RE.sub("", text)


# Target budget for a single image in a tool response.  The Claude CLI's
# stdio JSON buffer is 1 MB, and the API has its own token limits for tool
# results.  Keeping each image under 100 KB base64 (~75 KB raw) leaves
# plenty of room for text content alongside it.
_IMAGE_BUDGET_BYTES = 75_000

# Absolute ceiling — images larger than this after resizing are dropped
# entirely (e.g. a 10 000×10 000 screenshot).
_IMAGE_MAX_BYTES = 500_000

# Minimum dimension — never shrink below this (would lose all detail).
_IMAGE_MIN_DIM = 200

# Text mime type priority for LLM consumption.
# text/llm+plain is from https://github.com/rgbkrk/repr_llm — a repr designed
# specifically for language models. text/html is intentionally excluded: it's
# often bulky embedded JS (e.g. Plotly) that wastes context window.


def _fit_image_for_llm(
    data: bytes,
    mime: str,
    budget: int = _IMAGE_BUDGET_BYTES,
) -> bytes | None:
    """Resize an image so its raw bytes fit within *budget*.

    Uses Pillow (installed with matplotlib) to progressively shrink the
    image until it fits.  Returns ``None`` if the image can't be made
    small enough or Pillow isn't available.

    Only PNG and JPEG are resized; other formats are returned as-is if
    they already fit, or dropped.
    """
    if len(data) <= budget:
        return data

    try:
        from io import BytesIO

        from PIL import Image
    except ImportError:
        # No Pillow — return as-is if under the hard ceiling, else drop
        return data if len(data) <= _IMAGE_MAX_BYTES else None

    resizable = mime in ("image/png", "image/jpeg")
    if not resizable:
        return data if len(data) <= _IMAGE_MAX_BYTES else None

    try:
        img = Image.open(BytesIO(data))
    except Exception:
        return None

    save_fmt = "PNG" if mime == "image/png" else "JPEG"
    save_kwargs: dict = {}
    if save_fmt == "JPEG":
        save_kwargs["quality"] = 85

    # Progressively halve the longer dimension until we fit
    for _ in range(8):  # at most 8 halvings (256× reduction)
        w, h = img.size
        if w <= _IMAGE_MIN_DIM and h <= _IMAGE_MIN_DIM:
            break

        new_w = max(w // 2, _IMAGE_MIN_DIM)
        new_h = max(h // 2, _IMAGE_MIN_DIM)
        img = img.resize((new_w, new_h), Image.Resampling.LANCZOS)

        buf = BytesIO()
        img.save(buf, format=save_fmt, **save_kwargs)
        result = buf.getvalue()
        if len(result) <= budget:
            return result

    # Last attempt didn't fit — return it if under the hard ceiling
    buf = BytesIO()
    img.save(buf, format=save_fmt, **save_kwargs)
    result = buf.getvalue()
    return result if len(result) <= _IMAGE_MAX_BYTES else None


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
                    if isinstance(data, dict):
                        return json.dumps(data, indent=2)
                    if isinstance(data, str):
                        return json.dumps(json.loads(data), indent=2)
                    return json.dumps(data, indent=2)
                except (json.JSONDecodeError, TypeError):
                    return str(output.data[mime])
            val = output.data[mime]
            return val if isinstance(val, str) else str(val)
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

        # Images → ImageContent (resize to fit LLM context budget)
        for mime in ("image/png", "image/jpeg", "image/gif", "image/webp"):
            if mime in output.data:
                data = output.data[mime]
                if isinstance(data, bytes):
                    fitted = _fit_image_for_llm(data, mime)
                    if fitted is not None:
                        b64 = base64.b64encode(fitted).decode("ascii")
                        items.append(ImageContent(type="image", data=b64, mimeType=mime))
                elif isinstance(data, str):
                    # Legacy fallback: already base64-encoded string
                    raw = base64.b64decode(data)
                    fitted = _fit_image_for_llm(raw, mime)
                    if fitted is not None:
                        b64 = base64.b64encode(fitted).decode("ascii")
                        items.append(ImageContent(type="image", data=b64, mimeType=mime))

        # SVG as text (it's XML, not base64)
        if "image/svg+xml" in output.data:
            svg = output.data["image/svg+xml"]
            if isinstance(svg, str):
                items.append(TextContent(type="text", text=svg))

        # Best available text representation
        for mime in _TEXT_MIME_PRIORITY:
            if mime not in output.data:
                continue
            if mime == "application/json":
                try:
                    data = output.data[mime]
                    if isinstance(data, dict):
                        # Native dict from runtimed — serialize directly
                        text = json.dumps(data, indent=2)
                    elif isinstance(data, str):
                        text = json.dumps(json.loads(data), indent=2)
                    else:
                        text = json.dumps(data, indent=2)
                    items.append(TextContent(type="text", text=text))
                except (json.JSONDecodeError, TypeError):
                    items.append(TextContent(type="text", text=str(output.data[mime])))
            else:
                val = output.data[mime]
                items.append(
                    TextContent(type="text", text=val if isinstance(val, str) else str(val))
                )
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


def _build_cell_status_map(queue_state: QueueState) -> dict[str, str]:
    """Build a cell_id -> status mapping from queue state."""
    cell_status: dict[str, str] = {}
    if queue_state.executing:
        cell_status[queue_state.executing.cell_id] = "running"
    for entry in queue_state.queued:
        cell_status[entry.cell_id] = "queued"
    return cell_status


async def _get_cell_status_map(notebook: runtimed.Notebook) -> dict[str, str]:
    """Fetch queue state and return cell status map, empty on failure.

    Status is a best-effort annotation — errors should never prevent
    get_all_cells or get_cell from returning results.
    """
    try:
        queue_state = await notebook.queue_state()
        return _build_cell_status_map(queue_state)
    except asyncio.CancelledError:
        raise
    except Exception:
        return {}


async def _get_single_cell_status(notebook: runtimed.Notebook, cell_id: str) -> str | None:
    """Fetch queue status for a single cell, None on failure."""
    try:
        queue_state = await notebook.queue_state()
        if queue_state.executing and queue_state.executing.cell_id == cell_id:
            return "running"
        if any(entry.cell_id == cell_id for entry in queue_state.queued):
            return "queued"
        return None
    except asyncio.CancelledError:
        raise
    except Exception:
        return None


def _format_cell_summary(
    index: int,
    cell: runtimed.CellHandle,
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


def _format_cell(cell: runtimed.CellHandle, status: str | None = None) -> str:
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


def _cell_to_content(cell: runtimed.CellHandle, status: str | None = None) -> list[ContentItem]:
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
    client = _get_client()
    notebooks = await client.list_active_notebooks()
    return [
        {
            "notebook_id": info.notebook_id,
            "active_peers": info.active_peers,
            "has_runtime": info.has_runtime,
            "runtime_type": info.runtime_type,
            "status": info.status,
        }
        for info in notebooks
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
    ) -> dict[str, Any]:
        """Open the notebook in the nteract desktop app.

        The notebook must be currently running in the daemon. If no notebook_id
        is provided, opens the notebook from the current session.
        """
        target = notebook_id
        if target is None:
            if _notebook is not None:
                target = _notebook.notebook_id
            else:
                raise ValueError(
                    "No notebook_id provided and no active session. "
                    "Use list_active_notebooks() to find a notebook_id, or connect to one first."
                )

        client = _get_client()
        notebooks = await client.list_active_notebooks()
        notebook_ids = {info.notebook_id for info in notebooks}
        if target not in notebook_ids:
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
        return {"notebook_id": target, "opened": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def join_notebook(
    notebook_id: str,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Join an existing notebook session by ID.

    Use list_active_notebooks() to see available sessions. To open a file from disk,
    use open_notebook(path). To create a new notebook, use create_notebook().
    """
    global _notebook
    if ctx:
        _sniff_client_name(ctx)

    # Close existing notebook if any
    if _notebook is not None:
        with contextlib.suppress(Exception):
            await _notebook.close()

    # Join existing notebook
    client = _get_client()
    _notebook = await client.join_notebook(notebook_id, peer_label=_peer_label())

    return {
        "notebook_id": _notebook.notebook_id,
        "connected": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def open_notebook(path: str, ctx: Context | None = None) -> dict[str, Any]:
    """Open an existing .ipynb file. The kernel starts automatically.

    Use create_notebook() for new notebooks.
    """
    global _notebook
    if ctx:
        _sniff_client_name(ctx)

    if _notebook is not None:
        with contextlib.suppress(Exception):
            await _notebook.close()

    client = _get_client()
    _notebook = await client.open_notebook(path, peer_label=_peer_label())
    return {
        "notebook_id": _notebook.notebook_id,
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
    global _notebook
    if ctx:
        _sniff_client_name(ctx)

    if _notebook is not None:
        with contextlib.suppress(Exception):
            await _notebook.close()

    client = _get_client()
    _notebook = await client.create_notebook(
        runtime=runtime, working_dir=working_dir, peer_label=_peer_label()
    )

    if dependencies and runtime == "python":
        # Add dependencies to notebook metadata
        for dep in dependencies:
            await _notebook.add_dependency(dep)

        # The daemon may have auto-launched a kernel (without these deps).
        # Restart to ensure the kernel picks up the inline deps.
        with contextlib.suppress(Exception):
            await _notebook.restart()

    return {
        "notebook_id": _notebook.notebook_id,
        "runtime": runtime,
        "dependencies": dependencies or [],
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
async def save_notebook(path: str | None = None) -> dict[str, Any]:
    """Save notebook to disk.

    The daemon automatically re-keys ephemeral (UUID-based) rooms to the saved
    file path, so no disconnect/reconnect is needed.
    """
    notebook = await _get_notebook()
    old_notebook_id = notebook.notebook_id

    try:
        if path is not None:
            saved_path = await notebook.save_as(path)
        else:
            saved_path = await notebook.save()
    except Exception as e:
        error_msg = str(e)
        is_write_error = "Read-only" in error_msg or "Failed to write" in error_msg
        if is_write_error and path is None:
            raise RuntimeError(
                "No path specified. For notebooks created with create_notebook(), "
                "you must provide a path (e.g., save_notebook('/path/to/file.ipynb'))"
            ) from e
        raise

    new_notebook_id = notebook.notebook_id
    result: dict[str, Any] = {"path": saved_path, "notebook_id": new_notebook_id}
    if old_notebook_id != new_notebook_id:
        result["previous_notebook_id"] = old_notebook_id
    return result


# =============================================================================
# Kernel Management Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def interrupt_kernel() -> dict[str, Any]:
    """Interrupt the currently executing cell."""
    notebook = await _get_notebook()
    await notebook.interrupt()

    return {"interrupted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def restart_kernel(ctx: Context | None = None) -> dict[str, Any]:
    """Restart kernel, clearing all state. Use after dependency changes.

    Reports environment preparation progress (package downloads, installs)
    via MCP log notifications while waiting. Times out after 120s.
    """
    notebook = await _get_notebook()
    progress_messages: list[str] = await notebook.restart()

    # Send progress messages as MCP log notifications (retroactively)
    if ctx and progress_messages:
        for msg in progress_messages:
            with contextlib.suppress(Exception):
                await ctx.info(msg)

    return {
        "restarted": True,
        "env_source": notebook.runtime.kernel.env_source,
        "progress": progress_messages,
    }


# =============================================================================
# Dependency Management Tools
# =============================================================================


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def add_dependency(package: str) -> dict[str, Any]:
    """Add a package dependency (e.g. "pandas>=2.0"). Call sync_environment() to install."""
    notebook = await _get_notebook()
    deps = await notebook.add_dependency(package)
    return {"dependencies": deps, "added": package}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def remove_dependency(package: str) -> dict[str, Any]:
    """Remove a package dependency. Requires restart_kernel() to take effect."""
    notebook = await _get_notebook()
    deps = await notebook.remove_dependency(package)
    return {"dependencies": deps, "removed": package}


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_dependencies() -> dict[str, Any]:
    """Get the notebook's current package dependencies."""
    notebook = await _get_notebook()
    deps = await notebook.get_dependencies()
    return {"dependencies": deps}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def sync_environment() -> dict[str, Any]:
    """Hot-install new dependencies without restarting. Use restart_kernel() if this fails."""
    notebook = await _get_notebook()
    result = await notebook.sync_environment()
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
    notebook = await _get_notebook()
    if index is not None:
        cell = await notebook.cells.insert_at(index, source, cell_type)
    else:
        cell = await notebook.cells.create(source, cell_type)

    if and_run and cell_type == "code":
        return await _execute_cell_internal(cell.id, timeout_secs=timeout_secs)

    return [TextContent(type="text", text=f"Created cell: {cell.id}")]


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
    notebook = await _get_notebook()
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return TextContent(type="text", text=f'Cell "{cell_id}" not found')

    # Update source if provided
    if source is not None:
        await cell.set_source(source)

    # Update cell type if provided
    if cell_type is not None:
        await cell.set_type(cell_type)

    # If source was changed, ensure the final presence is cursor-at-end (not focus
    # from set_cell_type, which would clear the cursor on the frontend).
    if source is not None:
        await _send_edit_cursor(cell_id, source, len(source))

    # If nothing was changed, return current cell state
    if source is None and cell_type is None:
        return TextContent(type="text", text=f'Cell "{cell_id}" unchanged (no updates specified)')

    if and_run and cell.cell_type == "code":
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    return TextContent(type="text", text=f'Cell "{cell_id}" updated')


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def set_cells_source_hidden(
    cell_ids: Annotated[list[str], Field(description="IDs of cells to update")],
    hidden: Annotated[bool, Field(description="True to hide source, False to show")],
) -> TextContent:
    """Hide or show the source (code input) of one or more cells."""
    notebook = await _get_notebook()
    not_found: list[str] = []
    for cell_id in cell_ids:
        try:
            cell = notebook.cells.get_by_id(cell_id)
            await cell.set_source_hidden(hidden)
        except KeyError:
            not_found.append(cell_id)
    updated = len(cell_ids) - len(not_found)
    msg = f"Set source_hidden={hidden} on {updated} cell(s)"
    if not_found:
        msg += f"; not found: {not_found}"
    return TextContent(type="text", text=msg)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def set_cells_outputs_hidden(
    cell_ids: Annotated[list[str], Field(description="IDs of cells to update")],
    hidden: Annotated[bool, Field(description="True to hide outputs, False to show")],
) -> TextContent:
    """Hide or show the outputs of one or more cells."""
    notebook = await _get_notebook()
    not_found: list[str] = []
    for cell_id in cell_ids:
        try:
            cell = notebook.cells.get_by_id(cell_id)
            await cell.set_outputs_hidden(hidden)
        except KeyError:
            not_found.append(cell_id)
    updated = len(cell_ids) - len(not_found)
    msg = f"Set outputs_hidden={hidden} on {updated} cell(s)"
    if not_found:
        msg += f"; not found: {not_found}"
    return TextContent(type="text", text=msg)


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def add_cell_tags(
    cell_id: Annotated[str, Field(description="ID of the cell")],
    tags: Annotated[list[str], Field(description="Tags to add")],
) -> TextContent:
    """Add tags to a cell's metadata. Existing tags are preserved."""
    notebook = await _get_notebook()
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return TextContent(type="text", text=f"Cell {cell_id} not found")
    existing = cell.tags
    merged = existing + [t for t in tags if t not in existing]
    await cell.set_tags(merged)
    return TextContent(type="text", text=f"Tags for {cell_id}: {merged}")


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def remove_cell_tags(
    cell_id: Annotated[str, Field(description="ID of the cell")],
    tags: Annotated[list[str], Field(description="Tags to remove")],
) -> TextContent:
    """Remove tags from a cell's metadata."""
    notebook = await _get_notebook()
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return TextContent(type="text", text=f"Cell {cell_id} not found")
    existing = cell.tags
    filtered = [t for t in existing if t not in tags]
    await cell.set_tags(filtered)
    return TextContent(type="text", text=f"Tags for {cell_id}: {filtered}")


async def _send_edit_cursor(cell_id: str, source: str, offset: int) -> None:
    """Send cursor presence at a character offset (best-effort, non-blocking)."""
    if _notebook is None:
        return
    from nteract._editing import offset_to_line_col

    try:
        line, col = offset_to_line_col(source, offset)
        await _notebook.presence.set_cursor(cell_id, line, col)
    except Exception:
        pass  # Presence is best-effort — don't fail the edit


async def _send_cell_cursor(cell_id: str, line: int = 0, column: int = 0) -> None:
    """Send cursor presence on a cell (best-effort, errors silently ignored)."""
    if _notebook is None:
        return
    with contextlib.suppress(Exception):
        await _notebook.presence.set_cursor(cell_id, line, column)


async def _send_cell_focus(cell_id: str) -> None:
    """Send focus presence on a cell (best-effort, errors silently ignored)."""
    if _notebook is None:
        return
    with contextlib.suppress(Exception):
        await _notebook.presence.focus(cell_id)


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

    notebook = await _get_notebook()
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return TextContent(type="text", text=f'Cell "{cell_id}" not found')

    source = cell.source

    try:
        result = _replace_match(source, match, content, context_before, context_after)
    except PatternError as e:
        raise RuntimeError(f"{e} (match_count={e.match_count}, source_length={len(source)})") from e

    # Show cursor at edit location before applying
    await _send_edit_cursor(cell_id, source, result.span.start)

    await cell.splice(result.span.start, result.span.end - result.span.start, content)

    # Move cursor to end of replacement
    end_offset = result.span.start + len(content)
    await _send_edit_cursor(cell_id, result.new_source, end_offset)

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

    notebook = await _get_notebook()
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return TextContent(type="text", text=f'Cell "{cell_id}" not found')

    source = cell.source

    try:
        result = _replace_regex(source, pattern, content)
    except PatternError as e:
        raise RuntimeError(f"{e} (match_count={e.match_count}, source_length={len(source)})") from e

    # Show cursor at edit location before applying
    await _send_edit_cursor(cell_id, source, result.span.start)

    await cell.splice(result.span.start, result.span.end - result.span.start, content)

    # Move cursor to end of replacement
    end_offset = result.span.start + len(content)
    await _send_edit_cursor(cell_id, result.new_source, end_offset)

    if and_run:
        return await _execute_cell_internal(cell_id, timeout_secs=timeout_secs)

    diff = _format_edit_diff(cell_id, result.old_text, content)
    return TextContent(type="text", text=diff)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
async def get_cell(
    cell_id: str,
) -> list[ContentItem]:
    """Get a cell's source and outputs by ID."""
    notebook = await _get_notebook()
    await _send_cell_focus(cell_id)
    try:
        cell = notebook.cells.get_by_id(cell_id)
    except KeyError:
        return [TextContent(type="text", text=f'Cell "{cell_id}" not found')]
    status = await _get_single_cell_status(notebook, cell_id)
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

    notebook = await _get_notebook()
    all_cells = list(notebook.cells)

    # Fetch execution queue state to annotate running/queued cells
    cell_status = await _get_cell_status_map(notebook)

    # Apply pagination
    end = start + count if count is not None else len(all_cells)
    cells = all_cells[start:end]

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
            for item in _cell_to_content(cell, status=cell_status.get(cell.id)):
                # Skip images in bulk view — they blow up response size.
                # Use get_cell() to inspect individual cells with images.
                if isinstance(item, ImageContent):
                    items.append(TextContent(type="text", text=f"[image: {item.mimeType}]"))
                else:
                    items.append(item)
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
    notebook = await _get_notebook()
    cell = notebook.cells.get_by_id(cell_id)
    await cell.delete()
    return {"cell_id": cell_id, "deleted": True}


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def move_cell(
    cell_id: str,
    after_cell_id: Annotated[
        str | None, Field(description="Move after this cell, or null for start")
    ] = None,
) -> dict[str, Any]:
    """Move a cell to a new position."""
    notebook = await _get_notebook()
    cell = notebook.cells.get_by_id(cell_id)
    after = notebook.cells.get_by_id(after_cell_id) if after_cell_id else None
    await cell.move_after(after)
    return {
        "cell_id": cell_id,
        "after_cell_id": after_cell_id,
        "moved": True,
    }


@mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
async def clear_outputs(cell_id: str) -> dict[str, Any]:
    """Clear a cell's outputs."""
    notebook = await _get_notebook()
    cell = notebook.cells.get_by_id(cell_id)
    await cell.clear_outputs()
    return {"cell_id": cell_id, "cleared": True}


# =============================================================================
# Execution Tools
# =============================================================================


async def _execute_cell_internal(
    cell_id: str,
    timeout_secs: float = 30.0,
) -> list[ContentItem]:
    """Internal execution: queue, wait for outputs, read status from CRDT."""
    notebook = await _get_notebook()
    await _send_cell_focus(cell_id)
    cell = notebook.cells.get_by_id(cell_id)

    execution = await cell.execute()

    # execution.result() uses the Rust collect_outputs path which has a
    # confirm-sync retry loop — it waits for ExecutionDone scoped by
    # execution_id, then polls until outputs are materialized in the CRDT.
    # On timeout it raises — we fall back to reading partial outputs.
    # Real failures (transport, sync) are re-raised as tool errors.
    try:
        await execution.result(timeout_secs=timeout_secs)
    except asyncio.TimeoutError:
        pass  # partial results — read whatever is in the CRDT
    except Exception as exc:
        if "timed out" in str(exc).lower():
            pass  # RuntimedError timeout from Rust side
        else:
            raise

    # Status comes from the Execution handle (reads RuntimeStateDoc).
    # No business logic needed here — the handle knows the state.
    status = execution.status

    header = _format_header(cell.id, status=status, execution_count=cell.execution_count)
    items: list[ContentItem] = [TextContent(type="text", text=header)]
    items.extend(_outputs_to_content(cell.outputs))
    return items


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
    notebook = await _get_notebook()
    count = await notebook.run_all()
    return {"status": "queued", "count": count}


# =============================================================================
# Resources
# =============================================================================


@mcp.resource("notebook://cells")
async def resource_cells() -> str:
    """Get all cells in the current notebook as a compact summary."""
    if _notebook is None:
        return "Error: No active notebook"

    try:
        cell_status = await _get_cell_status_map(_notebook)
        lines = [
            _format_cell_summary(i, cell, status=cell_status.get(cell.id))
            for i, cell in enumerate(_notebook.cells)
        ]
        return "\n".join(lines)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cell/{cell_id}")
async def resource_cell(cell_id: str) -> str:
    """Get a specific cell's source and outputs."""
    if _notebook is None:
        return "Error: No active notebook"

    try:
        await _send_cell_focus(cell_id)
        cell = _notebook.cells.get_by_id(cell_id)
        status = await _get_single_cell_status(_notebook, cell_id)
        return _format_cell(cell, status=status)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cells/by-index/{index}")
async def resource_cell_by_index(index: int) -> str:
    """Get the cell at the specified position (0-based index)."""
    if _notebook is None:
        return "Error: No active notebook"

    try:
        num_cells = len(_notebook.cells)
        if index < 0 or index >= num_cells:
            return f"Error: Index {index} out of range (notebook has {num_cells} cells)"
        cell = _notebook.cells.get_by_index(index)
        await _send_cell_focus(cell.id)
        status = await _get_single_cell_status(_notebook, cell.id)
        return _format_cell(cell, status=status)
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://cell/{cell_id}/outputs")
async def resource_cell_outputs(cell_id: str) -> str:
    """Get a specific cell's outputs only (text format)."""
    if _notebook is None:
        return "Error: No active notebook"

    try:
        await _send_cell_focus(cell_id)
        cell = _notebook.cells.get_by_id(cell_id)
        output_text = _format_outputs_text(cell.outputs)
        if not output_text:
            return "(no outputs)"
        return output_text
    except Exception as e:
        return f"Error: {e}"


@mcp.resource("notebook://status")
async def resource_status() -> str:
    """Get the current notebook and runtime status as JSON."""
    if _notebook is None:
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
                "notebook_id": _notebook.notebook_id,
                "connected": await _notebook.is_connected(),
                "runtime_status": _notebook.runtime.kernel.status,
                "env_source": _notebook.runtime.kernel.env_source,
            }
        )
    except Exception as e:
        return json.dumps({"error": str(e)})


@mcp.resource("notebook://rooms")
async def resource_rooms() -> str:
    """Get all active notebook rooms as JSON."""
    try:
        client = _get_client()
        notebooks = await client.list_active_notebooks()
        return json.dumps(
            [
                {
                    "notebook_id": info.notebook_id,
                    "active_peers": info.active_peers,
                    "has_runtime": info.has_runtime,
                    "runtime_type": info.runtime_type,
                    "status": info.status,
                }
                for info in notebooks
            ]
        )
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
