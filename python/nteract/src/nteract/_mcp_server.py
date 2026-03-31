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

import argparse
import asyncio
import contextlib
import difflib
import json
import logging
import os
import re
import sys
from typing import Annotated, Any, Literal, NoReturn

from fastmcp import Context, FastMCP
from fastmcp.server.apps import AppConfig, ResourceCSP
from fastmcp.tools import ToolResult
from mcp.types import TextContent, ToolAnnotations
from pydantic import Field

import runtimed
from runtimed._internals import QueueState

logger = logging.getLogger(__name__)

# MCP content types for tool responses
ContentItem = TextContent

# ── CLI argument parsing ──────────────────────────────────────────────
# Parsed in main() so that importing the module doesn't blow up when the
# host process has its own argv (e.g. pytest).


class _StderrParser(argparse.ArgumentParser):
    """ArgumentParser that always writes to stderr (stdout is MCP's transport)."""

    def _print_message(self, message: str, file: Any = None) -> None:
        super()._print_message(message, file=sys.stderr)

    def exit(self, status: int = 0, message: str | None = None) -> NoReturn:
        if message:
            self._print_message(message, sys.stderr)
        raise SystemExit(status)


# Regex to strip ANSI escape sequences (terminal colors, cursor movement, etc.)
_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B")


def _strip_ansi(text: str) -> str:
    """Strip ANSI escape sequences from text.

    Kernel stream output (especially from pip/uv installs) often contains
    terminal control codes for colors, progress bars, and cursor movement.
    These waste LLM context and render as garbage in text responses.
    """
    return _ANSI_RE.sub("", text)


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

    All outputs are returned as TextContent. Image outputs use the
    text/llm+plain representation (a text description + blob URL synthesized
    by the daemon) rather than base64 image data.

    Mime priority for display_data/execute_result:
    - text/llm+plain, text/markdown, text/plain, application/json → TextContent
    - image/svg+xml → TextContent (XML text)
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


# ── MCP Apps widget ──────────────────────────────────────────────────

_WIDGET_RESOURCE_URI = "ui://nteract/output.html"
_WIDGET_MIME_TYPE = "text/html;profile=mcp-app"
_WIDGET_HTML_PATH = os.path.join(os.path.dirname(__file__), "_widget.html")


def _output_to_structured(output: runtimed.Output) -> dict[str, Any]:
    """Convert a runtimed Output to a JSON-serializable dict for the widget.

    Binary images are referenced by blob URL, keeping the payload small.
    The widget iframe's CSP must include the blob server origin via
    resourceDomains on the resource response for images to load.
    """
    if output.output_type == "stream":
        return {
            "output_type": "stream",
            "name": getattr(output, "name", "stdout"),
            "text": output.text or "",
        }

    if output.output_type == "error":
        return {
            "output_type": "error",
            "ename": output.ename or "",
            "evalue": output.evalue or "",
            "traceback": output.traceback or [],
        }

    # display_data or execute_result — use blob URLs for binary images.
    blob_urls = output.blob_urls or {}

    data: dict[str, Any] = {}
    if output.data:
        for mime, value in output.data.items():
            if mime == "text/llm+plain":
                continue  # LLM-specific, not for the widget
            if isinstance(value, bytes):
                # Use blob URL if available
                if mime in blob_urls:
                    data[mime] = blob_urls[mime]
                # else skip binary data with no blob URL
            elif isinstance(value, dict):
                data[mime] = value
            else:
                data[mime] = str(value)

    return {"output_type": output.output_type, "data": data}


def _build_cell_status_map(queue_state: QueueState) -> dict[str, str]:
    """Build a cell_id -> status mapping from queue state."""
    cell_status: dict[str, str] = {}
    if queue_state.executing:
        cell_status[queue_state.executing.cell_id] = "running"
    for entry in queue_state.queued:
        cell_status[entry.cell_id] = "queued"
    return cell_status


async def _get_cell_status_map(notebook: runtimed.Notebook) -> dict[str, str]:
    """Fetch queue state from daemon and return cell status map, empty on failure.

    Queries the daemon directly (not the local CRDT) so status is
    authoritative even right after execute_cell().
    """
    try:
        queue_state = await notebook._session.get_queue_state()
        return _build_cell_status_map(queue_state)
    except asyncio.CancelledError:
        raise
    except Exception:
        return {}


async def _get_single_cell_status(notebook: runtimed.Notebook, cell_id: str) -> str | None:
    """Fetch queue status for a single cell from daemon, None on failure."""
    try:
        queue_state = await notebook._session.get_queue_state()
        if queue_state.executing and queue_state.executing.cell_id == cell_id:
            return "running"
        if any(entry.cell_id == cell_id for entry in queue_state.queued):
            return "queued"
        return None
    except asyncio.CancelledError:
        raise
    except Exception:
        return None


def _read_runtime_info(notebook: runtimed.Notebook) -> dict[str, Any]:
    """Read runtime info snapshot from the notebook's local CRDT replica."""
    info: dict[str, Any] = {}
    try:
        rs = notebook.runtime
        kernel = rs.kernel

        info["kernel_status"] = kernel.status

        if kernel.language:
            info["language"] = kernel.language

        if kernel.name:
            info["kernel_name"] = kernel.name

        if kernel.env_source:
            info["env_source"] = kernel.env_source
            if kernel.env_source.startswith("conda:"):
                info["package_manager"] = "conda"
            elif kernel.env_source.startswith("uv:"):
                info["package_manager"] = "uv"
            elif kernel.env_source == "deno":
                info["package_manager"] = "deno"

        env = rs.env
        if not env.in_sync:
            info["env_in_sync"] = False

    except Exception:
        if not info:
            info["kernel_status"] = "unknown"

    return info


async def _collect_runtime_info(notebook: runtimed.Notebook) -> dict[str, Any]:
    """Collect runtime info, waiting briefly for the RuntimeStateDoc to sync.

    The daemon writes kernel status (e.g. "starting") to the RuntimeStateDoc
    after the peer joins, so there's a brief window where the client sees
    "not_started" even though the kernel is being launched. Poll for up to
    ~500ms to let the state doc catch up.
    """
    info = _read_runtime_info(notebook)
    if info.get("kernel_status") not in ("not_started", "unknown", ""):
        return info
    for _ in range(5):
        await asyncio.sleep(0.1)
        info = _read_runtime_info(notebook)
        if info.get("kernel_status") not in ("not_started", "unknown", ""):
            return info
    return info


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
    tags: list[str] | None = None,
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

    if tags:
        parts.append(f"tags: {', '.join(tags)}")

    parts.append("━━━")
    return " ".join(parts)


def _format_cell(cell: runtimed.CellHandle, status: str | None = None) -> str:
    """Format a cell for terminal display (includes source).

    Used by get_cell to show full cell state.
    """
    header = _format_header(
        cell.id,
        cell_type=cell.cell_type,
        status=status,
        execution_count=cell.execution_count,
        tags=cell.tags,
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
        cell.id,
        cell_type=cell.cell_type,
        status=status,
        execution_count=cell.execution_count,
        tags=cell.tags,
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
        result["data"] = {}
        if output.data:
            for mime in _TEXT_MIME_PRIORITY:
                if mime in output.data:
                    result["data"][mime] = output.data[mime]
                    break
        if output.output_type == "execute_result" and output.execution_count is not None:
            result["execution_count"] = output.execution_count

    return result


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


def _format_edit_diff(cell_id: str, old_text: str, new_text: str) -> str:
    """Format a unified diff for an edit operation."""
    old_lines = [line + "\n" for line in old_text.splitlines()]
    new_lines = [line + "\n" for line in new_text.splitlines()]
    diff = difflib.unified_diff(old_lines, new_lines, fromfile="before", tofile="after")
    diff_text = "".join(diff)
    return f'Edited cell "{cell_id}":\n{diff_text}'


# =============================================================================
# MCP Server
# =============================================================================


class NteractServer:
    """Encapsulates MCP server state and tool registration.

    All mutable state (notebook session, client, peer label) lives here
    instead of module globals. Tools are registered as closures that close
    over ``self``, so FastMCP sees clean function signatures.
    """

    def __init__(self, *, channel: str | None = None, no_show: bool = False):
        self.mcp = FastMCP("nteract")
        self._notebook: runtimed.Notebook | None = None
        self._client: runtimed.Client | None = None
        self._client_name: str | None = None
        self._channel: str | None = channel  # "stable", "nightly", or None

        # Resolve blob server URL for widget CSP (best-effort at init)
        self._widget_app_config = self._build_widget_app_config()

        self._register_tools(no_show=no_show)
        self._register_resources()

    # ── State helpers ─────────────────────────────────────────────────

    def _build_widget_app_config(self) -> AppConfig:
        """Build AppConfig with CSP for the widget resource."""
        port = self._resolve_blob_port()
        csp = None
        if port:
            csp = ResourceCSP(resource_domains=[f"http://localhost:{port}"])
        return AppConfig(csp=csp)

    @staticmethod
    def _resolve_blob_port() -> int | None:
        """Read blob port from daemon.json next to the socket."""
        try:
            sock = runtimed.default_socket_path()
            daemon_json = os.path.join(os.path.dirname(sock), "daemon.json")
            with open(daemon_json) as f:
                info = json.load(f)
            return info.get("blob_port")
        except Exception:
            return None

    def _get_client(self) -> runtimed.Client:
        if self._client is None:
            self._client = runtimed.Client()
        return self._client

    async def _get_notebook(self) -> runtimed.Notebook:
        if self._notebook is None:
            raise RuntimeError("No active notebook session. Call join_notebook first.")
        return self._notebook

    def _sniff_client_name(self, ctx: Context) -> None:
        if self._client_name is not None:
            return
        try:
            req_ctx = ctx.request_context
            if req_ctx is None:
                return
            params = req_ctx.session.client_params
            if params and params.clientInfo:
                info = params.clientInfo
                self._client_name = getattr(info, "title", None) or info.name
        except Exception:
            pass

    def _peer_label(self) -> str:
        return self._client_name or "Agent"

    async def _send_edit_cursor(self, cell_id: str, source: str, offset: int) -> None:
        if self._notebook is None:
            return
        from nteract._editing import offset_to_line_col

        try:
            line, col = offset_to_line_col(source, offset)
            await self._notebook.presence.set_cursor(cell_id, line, col)
        except Exception:
            pass

    async def _send_cell_cursor(self, cell_id: str, line: int = 0, column: int = 0) -> None:
        if self._notebook is None:
            return
        with contextlib.suppress(Exception):
            await self._notebook.presence.set_cursor(cell_id, line, column)

    async def _send_cell_focus(self, cell_id: str) -> None:
        if self._notebook is None:
            return
        with contextlib.suppress(Exception):
            await self._notebook.presence.focus(cell_id)

    def _show_app(self, notebook_path: str) -> None:
        """Launch the desktop app, respecting the channel override."""
        if self._channel is not None:
            runtimed.show_notebook_app_for_channel(self._channel, notebook_path)
        else:
            runtimed.show_notebook_app(notebook_path)

    async def _execute_cell_internal(self, cell_id: str, timeout_secs: float = 30.0) -> ToolResult:
        notebook = await self._get_notebook()
        # Don't emit focus here — it would overwrite any cursor presence
        # set by a prior edit (e.g. replace_match, set_cell). The cursor
        # from the edit should persist through execution.
        cell = notebook.cells.get_by_id(cell_id)

        execution = await cell.execute()

        try:
            await execution.result(timeout_secs=timeout_secs)
        except asyncio.TimeoutError:
            pass
        except Exception as exc:
            if "timed out" in str(exc).lower():
                pass
            else:
                raise

        status = execution.status
        header = _format_header(cell.id, status=status, execution_count=cell.execution_count)
        items: list[ContentItem] = [TextContent(type="text", text=header)]
        items.extend(_outputs_to_content(cell.outputs))

        # Build structured content for MCP Apps widget rendering
        structured_outputs = [_output_to_structured(o) for o in cell.outputs]
        structured_content: dict[str, Any] = {
            "cell": {
                "cell_id": cell.id,
                "source": cell.source or "",
                "outputs": structured_outputs,
                "execution_count": cell.execution_count,
                "status": status or "idle",
            }
        }

        return ToolResult(
            content=items,
            structured_content=structured_content,
        )

    def cleanup(self) -> None:
        """Best-effort cleanup of the active notebook session."""
        nb = self._notebook
        self._notebook = None
        self._client = None
        if nb is not None:
            try:
                try:
                    loop = asyncio.get_running_loop()
                except RuntimeError:
                    loop = None

                if loop is not None and loop.is_running():
                    loop.create_task(nb.disconnect())
                else:
                    asyncio.run(nb.disconnect())
            except Exception:
                pass

    # ── Tool registration ─────────────────────────────────────────────

    def _register_tools(self, *, no_show: bool = False) -> None:  # noqa: C901
        srv = self
        _tool_app = AppConfig(resource_uri=_WIDGET_RESOURCE_URI)

        # -- Session management --

        @srv.mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
        async def list_active_notebooks() -> list[dict[str, Any]]:
            """List all open notebook sessions.

            Returns notebooks currently open by users or other agents.
            Use join_notebook(notebook_id) to connect to one.
            """
            client = srv._get_client()
            notebooks = await client.list_active_notebooks()
            return [
                {
                    "notebook_id": info.notebook_id,
                    "active_peers": info.active_peers,
                    "has_runtime": info.has_runtime,
                    "runtime_type": info.runtime_type,
                    "status": info.status,
                    "is_draining": info.is_draining,
                }
                for info in notebooks
            ]

        if not no_show:

            @srv.mcp.tool()
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
                    if srv._notebook is not None:
                        target = srv._notebook.notebook_id
                    else:
                        raise ValueError(
                            "No notebook_id provided and no active session. "
                            "Use list_active_notebooks() to find a notebook_id, "
                            "or connect to one first."
                        )

                client = srv._get_client()
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

                srv._show_app(target)
                return {"notebook_id": target, "opened": True}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
        async def join_notebook(
            notebook_id: str,
            ctx: Context | None = None,
        ) -> dict[str, Any]:
            """Join an existing notebook session by ID.

            Use list_active_notebooks() to see available sessions. To open a file
            from disk, use open_notebook(path). To create a new notebook, use
            create_notebook().
            """
            if ctx:
                srv._sniff_client_name(ctx)

            if srv._notebook is not None:
                with contextlib.suppress(Exception):
                    await srv._notebook.disconnect()

            client = srv._get_client()
            srv._notebook = await client.join_notebook(notebook_id, peer_label=srv._peer_label())

            cell_status = await _get_cell_status_map(srv._notebook)
            lines = [
                _format_cell_summary(
                    i,
                    cell,
                    preview_chars=60,
                    include_outputs=False,
                    status=cell_status.get(cell.id),
                )
                for i, cell in enumerate(srv._notebook.cells)
            ]

            runtime_info = await _collect_runtime_info(srv._notebook)
            deps: list[str] = []
            with contextlib.suppress(Exception):
                deps = await srv._notebook.get_dependencies()

            return {
                "notebook_id": srv._notebook.notebook_id,
                "connected": True,
                "runtime": runtime_info,
                "dependencies": deps,
                "cells": "\n".join(lines),
            }

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
        async def open_notebook(path: str, ctx: Context | None = None) -> dict[str, Any]:
            """Open an existing .ipynb file. The kernel starts automatically.

            Use create_notebook() for new notebooks.
            """
            if ctx:
                srv._sniff_client_name(ctx)

            if srv._notebook is not None:
                with contextlib.suppress(Exception):
                    await srv._notebook.disconnect()

            client = srv._get_client()
            srv._notebook = await client.open_notebook(path, peer_label=srv._peer_label())

            cell_status = await _get_cell_status_map(srv._notebook)
            lines = [
                _format_cell_summary(
                    i,
                    cell,
                    preview_chars=60,
                    include_outputs=False,
                    status=cell_status.get(cell.id),
                )
                for i, cell in enumerate(srv._notebook.cells)
            ]

            runtime_info = await _collect_runtime_info(srv._notebook)
            deps: list[str] = []
            with contextlib.suppress(Exception):
                deps = await srv._notebook.get_dependencies()

            return {
                "notebook_id": srv._notebook.notebook_id,
                "path": path,
                "runtime": runtime_info,
                "dependencies": deps,
                "cells": "\n".join(lines),
            }

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
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
                    description="Python packages to pre-install"
                    " (e.g. ['pandas', 'requests'])."
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
            if ctx:
                srv._sniff_client_name(ctx)

            if srv._notebook is not None:
                with contextlib.suppress(Exception):
                    await srv._notebook.disconnect()

            client = srv._get_client()
            srv._notebook = await client.create_notebook(
                runtime=runtime,
                working_dir=working_dir,
                peer_label=srv._peer_label(),
                dependencies=dependencies if runtime == "python" else None,
            )

            if dependencies and runtime == "python":
                with contextlib.suppress(Exception):
                    await srv._notebook.restart()

            runtime_info = await _collect_runtime_info(srv._notebook)
            if "language" not in runtime_info:
                runtime_info["language"] = runtime

            return {
                "notebook_id": srv._notebook.notebook_id,
                "runtime": runtime_info,
                "dependencies": dependencies or [],
            }

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=False))
        async def save_notebook(path: str | None = None) -> dict[str, Any]:
            """Save notebook to disk.

            The daemon automatically re-keys ephemeral (UUID-based) rooms to the
            saved file path, so no disconnect/reconnect is needed.
            """
            notebook = await srv._get_notebook()
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
                        "you must provide a path"
                        " (e.g., save_notebook('/path/to/file.ipynb'))"
                    ) from e
                raise

            new_notebook_id = notebook.notebook_id
            result: dict[str, Any] = {"path": saved_path, "notebook_id": new_notebook_id}
            if old_notebook_id != new_notebook_id:
                result["previous_notebook_id"] = old_notebook_id
            return result

        # -- Kernel management --

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def interrupt_kernel() -> dict[str, Any]:
            """Interrupt the currently executing cell."""
            notebook = await srv._get_notebook()
            await notebook.interrupt()
            return {"interrupted": True}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def restart_kernel(ctx: Context | None = None) -> dict[str, Any]:
            """Restart kernel, clearing all state. Use after dependency changes.

            Reports environment preparation progress (package downloads, installs)
            via MCP log notifications while waiting. Times out after 120s.
            """
            notebook = await srv._get_notebook()
            progress_messages: list[str] = await notebook.restart()

            if ctx and progress_messages:
                for msg in progress_messages:
                    with contextlib.suppress(Exception):
                        await ctx.info(msg)

            return {
                "restarted": True,
                "env_source": notebook.runtime.kernel.env_source,
                "progress": progress_messages,
            }

        # -- Dependency management --

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def add_dependency(package: str) -> dict[str, Any]:
            """Add a package dependency (e.g. "pandas>=2.0").

            Call sync_environment() to install. To upgrade a package,
            just add the newer version — no need to remove_dependency
            first. A restart_kernel is still needed after sync.
            """
            notebook = await srv._get_notebook()
            deps = await notebook.add_dependency(package)
            return {"dependencies": deps, "added": package}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def remove_dependency(package: str) -> dict[str, Any]:
            """Remove a package dependency. Requires restart_kernel() to take effect."""
            notebook = await srv._get_notebook()
            deps = await notebook.remove_dependency(package)
            return {"dependencies": deps, "removed": package}

        @srv.mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
        async def get_dependencies() -> dict[str, Any]:
            """Get the notebook's current package dependencies."""
            notebook = await srv._get_notebook()
            deps = await notebook.get_dependencies()
            return {"dependencies": deps}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def sync_environment() -> dict[str, Any]:
            """Hot-install new dependencies without restarting.

            Use restart_kernel() if this fails.
            """
            notebook = await srv._get_notebook()
            result = await notebook.sync_environment()
            return {
                "success": result.success,
                "synced_packages": result.synced_packages,
                "error": result.error,
                "needs_restart": result.needs_restart,
            }

        # -- Cell operations --

        @srv.mcp.tool(
            annotations=ToolAnnotations(destructiveHint=False),
            app=_tool_app,
        )
        async def create_cell(
            source: str = "",
            cell_type: Literal["code", "markdown", "raw"] = "code",
            index: Annotated[
                int | None, Field(description="Position to insert. None appends at end")
            ] = None,
            and_run: Annotated[
                bool, Field(description="Execute the cell immediately after creation")
            ] = False,
            timeout_secs: Annotated[
                float, Field(description="Max seconds to wait for execution")
            ] = 30.0,
        ) -> ToolResult:
            """Create a cell, optionally executing it."""
            notebook = await srv._get_notebook()
            if index is not None:
                cell = await notebook.cells.insert_at(index, source, cell_type)
            else:
                cell = await notebook.cells.create(source, cell_type)

            if and_run and cell_type == "code":
                return await srv._execute_cell_internal(cell.id, timeout_secs=timeout_secs)

            return ToolResult(content=[TextContent(type="text", text=f"Created cell: {cell.id}")])

        @srv.mcp.tool(
            annotations=ToolAnnotations(destructiveHint=True),
            app=_tool_app,
        )
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
            timeout_secs: Annotated[
                float, Field(description="Max seconds to wait for execution")
            ] = 30.0,
        ) -> ToolResult:
            """Update a cell's source and/or type. Use replace_match for targeted edits."""
            notebook = await srv._get_notebook()
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return ToolResult(
                    content=[TextContent(type="text", text=f'Cell "{cell_id}" not found')]
                )

            if source is not None:
                await cell.set_source(source)
            if cell_type is not None:
                await cell.set_type(cell_type)
            if source is not None:
                await srv._send_edit_cursor(cell_id, source, len(source))

            if source is None and cell_type is None:
                return ToolResult(
                    content=[
                        TextContent(
                            type="text", text=f'Cell "{cell_id}" unchanged (no updates specified)'
                        )
                    ]
                )

            if and_run and cell.cell_type == "code":
                return await srv._execute_cell_internal(cell_id, timeout_secs=timeout_secs)

            return ToolResult(content=[TextContent(type="text", text=f'Cell "{cell_id}" updated')])

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def set_cells_source_hidden(
            cell_ids: Annotated[list[str], Field(description="IDs of cells to update")],
            hidden: Annotated[bool, Field(description="True to hide source, False to show")],
        ) -> TextContent:
            """Hide or show the source (code input) of one or more cells."""
            notebook = await srv._get_notebook()
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

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def set_cells_outputs_hidden(
            cell_ids: Annotated[list[str], Field(description="IDs of cells to update")],
            hidden: Annotated[bool, Field(description="True to hide outputs, False to show")],
        ) -> TextContent:
            """Hide or show the outputs of one or more cells."""
            notebook = await srv._get_notebook()
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

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def add_cell_tags(
            cell_id: Annotated[str, Field(description="ID of the cell")],
            tags: Annotated[list[str], Field(description="Tags to add")],
        ) -> TextContent:
            """Add tags to a cell's metadata. Existing tags are preserved."""
            notebook = await srv._get_notebook()
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return TextContent(type="text", text=f"Cell {cell_id} not found")
            existing = cell.tags
            merged = existing + [t for t in tags if t not in existing]
            await cell.set_tags(merged)
            return TextContent(type="text", text=f"Tags for {cell_id}: {merged}")

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def remove_cell_tags(
            cell_id: Annotated[str, Field(description="ID of the cell")],
            tags: Annotated[list[str], Field(description="Tags to remove")],
        ) -> TextContent:
            """Remove tags from a cell's metadata."""
            notebook = await srv._get_notebook()
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return TextContent(type="text", text=f"Cell {cell_id} not found")
            existing = cell.tags
            filtered = [t for t in existing if t not in tags]
            await cell.set_tags(filtered)
            return TextContent(type="text", text=f"Tags for {cell_id}: {filtered}")

        @srv.mcp.tool(
            annotations=ToolAnnotations(destructiveHint=True),
            app=_tool_app,
        )
        async def replace_match(
            cell_id: str,
            match: Annotated[
                str, Field(description="Literal text to find (must match exactly once)")
            ],
            content: Annotated[
                str, Field(description="Literal replacement text — real newlines, no escapes")
            ],
            context_before: Annotated[
                str, Field(description="Text that must appear before the match")
            ] = "",
            context_after: Annotated[
                str, Field(description="Text that must appear after the match")
            ] = "",
            and_run: Annotated[
                bool, Field(description="Execute the cell immediately after edit")
            ] = False,
            timeout_secs: Annotated[
                float, Field(description="Max seconds to wait for execution")
            ] = 30.0,
        ) -> ToolResult:
            """Replace matched text in a cell. Prefer this for simple, targeted edits.

            Use context_before/context_after to disambiguate when match appears
            multiple times. Fails if 0 or >1 matches (reports count + offsets).
            Use replace_regex for zero-width insertions or structural patterns.
            """
            from nteract._editing import PatternError
            from nteract._editing import replace_match as _replace_match

            notebook = await srv._get_notebook()
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return ToolResult(
                    content=[TextContent(type="text", text=f'Cell "{cell_id}" not found')]
                )

            source = cell.source
            try:
                result = _replace_match(source, match, content, context_before, context_after)
            except PatternError as e:
                raise RuntimeError(
                    f"{e} (match_count={e.match_count}, source_length={len(source)})"
                ) from e

            await srv._send_edit_cursor(cell_id, source, result.span.start)
            await cell.splice(result.span.start, result.span.end - result.span.start, content)
            end_offset = result.span.start + len(content)
            await srv._send_edit_cursor(cell_id, result.new_source, end_offset)

            if and_run:
                return await srv._execute_cell_internal(cell_id, timeout_secs=timeout_secs)

            diff = _format_edit_diff(cell_id, result.old_text, content)
            return ToolResult(content=[TextContent(type="text", text=diff)])

        @srv.mcp.tool(
            annotations=ToolAnnotations(destructiveHint=True),
            app=_tool_app,
        )
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
                str,
                Field(
                    description="Literal replacement text — not re.sub syntax, no backreferences"
                ),
            ],
            and_run: Annotated[
                bool, Field(description="Execute the cell immediately after edit")
            ] = False,
            timeout_secs: Annotated[
                float, Field(description="Max seconds to wait for execution")
            ] = 30.0,
        ) -> ToolResult:
            """Replace a regex-matched span. Use for anchors, lookarounds, or
            zero-width insertions.

            Fails if 0 or >1 matches (reports count + offsets for disambiguation).
            """
            from nteract._editing import PatternError
            from nteract._editing import replace_regex as _replace_regex

            notebook = await srv._get_notebook()
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return ToolResult(
                    content=[TextContent(type="text", text=f'Cell "{cell_id}" not found')]
                )

            source = cell.source
            try:
                result = _replace_regex(source, pattern, content)
            except PatternError as e:
                raise RuntimeError(
                    f"{e} (match_count={e.match_count}, source_length={len(source)})"
                ) from e

            await srv._send_edit_cursor(cell_id, source, result.span.start)
            await cell.splice(result.span.start, result.span.end - result.span.start, content)
            end_offset = result.span.start + len(content)
            await srv._send_edit_cursor(cell_id, result.new_source, end_offset)

            if and_run:
                return await srv._execute_cell_internal(cell_id, timeout_secs=timeout_secs)

            diff = _format_edit_diff(cell_id, result.old_text, content)
            return ToolResult(content=[TextContent(type="text", text=diff)])

        # -- Cell retrieval --

        @srv.mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
        async def get_cell(cell_id: str) -> list[ContentItem]:
            """Get a cell's source and outputs by ID."""
            notebook = await srv._get_notebook()
            await srv._send_cell_focus(cell_id)
            try:
                cell = notebook.cells.get_by_id(cell_id)
            except KeyError:
                return [TextContent(type="text", text=f'Cell "{cell_id}" not found')]
            status = await _get_single_cell_status(notebook, cell_id)
            return _cell_to_content(cell, status=status)

        @srv.mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
        async def get_all_cells(
            format: Annotated[
                Literal["summary", "json", "rich"],
                Field(description="'summary' (default), 'json', or 'rich' for full text content"),
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
                    "rich" for full content as text (image outputs use text/llm+plain
                    descriptions; use the blob URL in the description to read the
                    image directly if needed).
                start: Starting cell index for pagination.
                count: Number of cells to return (None = all remaining).
                include_outputs: Include output previews in summary format.
                preview_chars: Max characters for source/output preview in summary format.
            """
            if start < 0:
                raise ValueError(f"start must be >= 0, got {start}")
            if count is not None and count < 0:
                raise ValueError(f"count must be >= 0 or None, got {count}")
            if preview_chars < 1:
                raise ValueError(f"preview_chars must be >= 1, got {preview_chars}")

            notebook = await srv._get_notebook()
            all_cells = list(notebook.cells)
            cell_status = await _get_cell_status_map(notebook)

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
                        "tags": list(cell.tags),
                    }
                    for cell in cells
                ]

            if format == "rich":
                items: list[ContentItem] = []
                for cell in cells:
                    items.extend(_cell_to_content(cell, status=cell_status.get(cell.id)))
                return items

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

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def delete_cell(cell_id: str) -> dict[str, Any]:
            """Delete a cell by ID."""
            notebook = await srv._get_notebook()
            cell = notebook.cells.get_by_id(cell_id)
            await cell.delete()
            return {"cell_id": cell_id, "deleted": True}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def move_cell(
            cell_id: str,
            after_cell_id: Annotated[
                str | None, Field(description="Move after this cell, or null for start")
            ] = None,
        ) -> dict[str, Any]:
            """Move a cell to a new position."""
            notebook = await srv._get_notebook()
            cell = notebook.cells.get_by_id(cell_id)
            after = notebook.cells.get_by_id(after_cell_id) if after_cell_id else None
            await cell.move_after(after)
            return {"cell_id": cell_id, "after_cell_id": after_cell_id, "moved": True}

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def clear_outputs(cell_id: str) -> dict[str, Any]:
            """Clear a cell's outputs."""
            notebook = await srv._get_notebook()
            cell = notebook.cells.get_by_id(cell_id)
            await cell.clear_outputs()
            return {"cell_id": cell_id, "cleared": True}

        # -- Execution --

        @srv.mcp.tool(
            annotations=ToolAnnotations(destructiveHint=True),
            app=_tool_app,
        )
        async def execute_cell(
            cell_id: str,
            timeout_secs: Annotated[
                float,
                Field(description="Max seconds to wait; returns partial results if exceeded"),
            ] = 30.0,
        ) -> ToolResult:
            """Execute a cell. Returns partial results if timeout exceeded."""
            return await srv._execute_cell_internal(cell_id, timeout_secs=timeout_secs)

        @srv.mcp.tool(annotations=ToolAnnotations(destructiveHint=True))
        async def run_all_cells() -> dict[str, Any]:
            """Queue all code cells for execution. Use get_all_cells() to see results."""
            notebook = await srv._get_notebook()
            count = await notebook.run_all()
            return {"status": "queued", "count": count}

    # ── Resource registration ─────────────────────────────────────────

    def _register_resources(self) -> None:
        srv = self

        # MCP Apps widget resource — serves the output renderer UI.
        # AppConfig with ResourceCSP allows the iframe to load images
        # from the daemon's blob HTTP server.
        @srv.mcp.resource(
            _WIDGET_RESOURCE_URI,
            name="nteract output widget",
            mime_type=_WIDGET_MIME_TYPE,
            app=srv._widget_app_config,
        )
        async def widget_resource() -> str:
            """Interactive output renderer for notebook cells."""
            try:
                with open(_WIDGET_HTML_PATH, encoding="utf-8") as f:
                    return f.read()
            except FileNotFoundError:
                return (
                    "<html><body>Widget not built."
                    " Run: cd apps/mcp-app && npm run build</body></html>"
                )

        @srv.mcp.resource("notebook://cells")
        async def resource_cells() -> str:
            """Get all cells in the current notebook as a compact summary."""
            if srv._notebook is None:
                return "Error: No active notebook"
            try:
                cell_status = await _get_cell_status_map(srv._notebook)
                lines = [
                    _format_cell_summary(i, cell, status=cell_status.get(cell.id))
                    for i, cell in enumerate(srv._notebook.cells)
                ]
                return "\n".join(lines)
            except Exception as e:
                return f"Error: {e}"

        @srv.mcp.resource("notebook://cell/{cell_id}")
        async def resource_cell(cell_id: str) -> str:
            """Get a specific cell's source and outputs."""
            if srv._notebook is None:
                return "Error: No active notebook"
            try:
                await srv._send_cell_focus(cell_id)
                cell = srv._notebook.cells.get_by_id(cell_id)
                status = await _get_single_cell_status(srv._notebook, cell_id)
                return _format_cell(cell, status=status)
            except Exception as e:
                return f"Error: {e}"

        @srv.mcp.resource("notebook://cells/by-index/{index}")
        async def resource_cell_by_index(index: int) -> str:
            """Get the cell at the specified position (0-based index)."""
            if srv._notebook is None:
                return "Error: No active notebook"
            try:
                num_cells = len(srv._notebook.cells)
                if index < 0 or index >= num_cells:
                    return f"Error: Index {index} out of range (notebook has {num_cells} cells)"
                cell = srv._notebook.cells.get_by_index(index)
                await srv._send_cell_focus(cell.id)
                status = await _get_single_cell_status(srv._notebook, cell.id)
                return _format_cell(cell, status=status)
            except Exception as e:
                return f"Error: {e}"

        @srv.mcp.resource("notebook://cell/{cell_id}/outputs")
        async def resource_cell_outputs(cell_id: str) -> str:
            """Get a specific cell's outputs only (text format)."""
            if srv._notebook is None:
                return "Error: No active notebook"
            try:
                await srv._send_cell_focus(cell_id)
                cell = srv._notebook.cells.get_by_id(cell_id)
                output_text = _format_outputs_text(cell.outputs)
                if not output_text:
                    return "(no outputs)"
                return output_text
            except Exception as e:
                return f"Error: {e}"

        @srv.mcp.resource("notebook://status")
        async def resource_status() -> str:
            """Get the current notebook and runtime status as JSON."""
            if srv._notebook is None:
                return json.dumps({"connected": False, "kernel_started": False, "env_source": None})
            try:
                return json.dumps(
                    {
                        "notebook_id": srv._notebook.notebook_id,
                        "connected": srv._notebook.is_connected,
                        "runtime_status": srv._notebook.runtime.kernel.status,
                        "env_source": srv._notebook.runtime.kernel.env_source,
                    }
                )
            except Exception as e:
                return json.dumps({"error": str(e)})

        @srv.mcp.resource("notebook://rooms")
        async def resource_rooms() -> str:
            """Get all active notebook rooms as JSON."""
            try:
                client = srv._get_client()
                notebooks = await client.list_active_notebooks()
                return json.dumps(
                    [
                        {
                            "notebook_id": info.notebook_id,
                            "active_peers": info.active_peers,
                            "has_runtime": info.has_runtime,
                            "runtime_type": info.runtime_type,
                            "status": info.status,
                            "is_draining": info.is_draining,
                        }
                        for info in notebooks
                    ]
                )
            except Exception as e:
                return json.dumps({"error": str(e)})


# =============================================================================
# Entry Point
# =============================================================================


def _find_runt_binary(channel: str) -> str | None:
    """Find the runt binary using the same resolution as mcpb/server/launch.js.

    Search order:
    1. PATH (covers /usr/local/bin/ where the app installer puts the binary)
    2. Platform-specific app bundle / install locations
    """
    import platform
    import shutil

    binary_name = "runt-nightly" if channel == "nightly" else "runt"
    app_bundle_names = (
        ["nteract Nightly", "nteract-nightly", "nteract (Nightly)"]
        if channel == "nightly"
        else ["nteract"]
    )

    # 1. Check PATH
    found = shutil.which(binary_name)
    if found:
        return found

    # 2. Check platform-specific sidecar / install paths
    home = os.path.expanduser("~")
    system = platform.system()

    candidates: list[str] = []
    if system == "Darwin":
        for name in app_bundle_names:
            candidates.append(f"/Applications/{name}.app/Contents/MacOS/{binary_name}")
            candidates.append(
                os.path.join(home, f"Applications/{name}.app/Contents/MacOS/{binary_name}")
            )
    elif system == "Windows":
        local_app_data = os.environ.get("LOCALAPPDATA", os.path.join(home, "AppData", "Local"))
        for name in app_bundle_names:
            candidates.append(os.path.join(local_app_data, name, f"{binary_name}.exe"))
            candidates.append(os.path.join(local_app_data, "Programs", name, f"{binary_name}.exe"))
    else:  # Linux
        candidates.append(os.path.join(home, ".local", "bin", binary_name))
        for name in app_bundle_names:
            slug = name.lower().replace(" ", "-")
            candidates.append(f"/usr/share/{slug}/{binary_name}")
            candidates.append(f"/opt/{slug}/{binary_name}")

    for path in candidates:
        if os.path.isfile(path):
            return path

    return None


def main():
    """Launch the nteract MCP server.

    Finds and exec's the installed ``runt mcp`` binary (shipped with the
    nteract desktop app). Falls back to the built-in Python MCP server if
    ``runt`` is not installed (``--legacy`` flag forces this).
    """
    parser = _StderrParser(
        prog="nteract",
        description="nteract MCP server — AI-powered Jupyter notebooks.",
    )
    parser.add_argument(
        "--version",
        action="store_true",
        help="Print version and exit.",
    )
    channel_group = parser.add_mutually_exclusive_group()
    channel_group.add_argument(
        "--nightly",
        action="store_true",
        help="Connect to the nteract nightly daemon and open nightly app.",
    )
    channel_group.add_argument(
        "--stable",
        action="store_true",
        help="Connect to the nteract stable daemon and open stable app.",
    )
    parser.add_argument(
        "--no-show",
        action="store_true",
        help="Disable the show_notebook tool (headless environments).",
    )
    parser.add_argument(
        "--legacy",
        action="store_true",
        help="Use the built-in Python MCP server instead of runt mcp.",
    )
    args = parser.parse_args()

    if args.version:
        from importlib.metadata import version

        print(f"nteract {version('nteract')}", file=sys.stderr)
        raise SystemExit(0)

    channel = "nightly" if args.nightly else "stable"

    # Default path: find and exec the Rust MCP server
    if not args.legacy:
        binary = _find_runt_binary(channel)
        if binary:
            runt_args = [binary, "mcp"]
            if args.no_show:
                runt_args.append("--no-show")
            print(f"Launching {' '.join(runt_args)}", file=sys.stderr)
            os.execvp(binary, runt_args)
            # execvp never returns
        else:
            binary_name = "runt-nightly" if channel == "nightly" else "runt"
            app_name = "nteract Nightly" if channel == "nightly" else "nteract"
            print(
                f"Error: {binary_name} not found.\n\n"
                f"Install {app_name} from https://nteract.io to use this MCP server.\n"
                f"The app puts {binary_name} on your PATH during installation.\n"
                f"\n"
                f"To use the built-in Python MCP server instead, run:\n"
                f"  nteract --legacy\n",
                file=sys.stderr,
            )
            raise SystemExit(1)

    # Legacy path: run the built-in Python MCP server directly
    if (args.nightly or args.stable) and not os.environ.get("RUNTIMED_SOCKET_PATH"):
        os.environ["RUNTIMED_SOCKET_PATH"] = runtimed.socket_path_for_channel(channel)

    server = NteractServer(channel=channel, no_show=args.no_show)

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        stream=sys.stderr,
    )
    import atexit

    atexit.register(server.cleanup)
    try:
        server.mcp.run(transport="stdio")
    except KeyboardInterrupt:
        sys.exit(130)
    finally:
        server.cleanup()


if __name__ == "__main__":
    main()
