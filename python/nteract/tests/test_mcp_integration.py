"""Integration tests for nteract MCP server.

These tests require a running runtimed daemon. They use MCP's in-memory
transport (anyio memory streams) to test the server without stdio.

Run locally:
    # Start runtimed daemon first
    runtimed

    # Run tests
    uv run pytest tests/test_mcp_integration.py -v

Skip in CI (requires daemon):
    uv run pytest tests/ -v --ignore=tests/test_mcp_integration.py
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import re
from typing import Any

import anyio
import pytest
from mcp import ClientSession

from nteract._mcp_server import mcp


@pytest.fixture
async def mcp_client():
    """Create in-memory MCP client connected to our server.

    Uses asyncio for task management to avoid anyio cancel scope
    issues with pytest-asyncio fixture teardown.
    """
    # Bidirectional streams: client->server and server->client
    client_send, server_recv = anyio.create_memory_object_stream[Any](0)
    server_send, client_recv = anyio.create_memory_object_stream[Any](0)

    # Start server as asyncio task (not anyio task group)
    server_task = asyncio.create_task(
        mcp._mcp_server.run(
            server_recv,
            server_send,
            mcp._mcp_server.create_initialization_options(),
            raise_exceptions=True,
        )
    )

    # Give server a moment to start
    await asyncio.sleep(0.01)

    # Create client - use manual __aenter__/__aexit__ to control cleanup
    client = ClientSession(client_recv, client_send)
    await client.__aenter__()

    try:
        await client.initialize()
        yield client
    finally:
        # Cancel server first
        server_task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await server_task

        # Close streams to unblock any pending operations
        await client_send.aclose()
        await server_send.aclose()

        # Best-effort client cleanup - suppress task context errors
        with contextlib.suppress(RuntimeError):
            await client.__aexit__(None, None, None)


def _get_text(result: Any) -> str:
    """Get all text content from MCP tool result.

    Joins text from all TextContent items in the response, since tools
    now return multiple content items (header, outputs, images, etc.).
    """
    if not hasattr(result, "content") or not result.content:
        return ""
    parts = []
    for item in result.content:
        if hasattr(item, "text"):
            parts.append(item.text)
    return "\n\n".join(parts)


def _parse_json(result: Any) -> dict[str, Any]:
    """Parse JSON from MCP tool result (for tools that return JSON)."""
    text = _get_text(result)
    if text:
        return json.loads(text)
    return {}


def _extract_cell_id(text: str) -> str | None:
    """Extract cell ID from 'Created cell: {id}' or header format."""
    # Match "Created cell: cell-uuid"
    match = re.search(r"Created cell: (cell-[\w-]+)", text)
    if match:
        return match.group(1)
    # Match header format "━━━ cell-uuid" (full ID in header)
    match = re.search(r"━━━ (cell-[\w-]+)", text)
    if match:
        return match.group(1)
    return None


@pytest.mark.asyncio
async def test_list_tools(mcp_client: ClientSession):
    """Verify all expected tools are exposed."""
    tools = await mcp_client.list_tools()
    tool_names = {t.name for t in tools.tools}

    # Core tools should be present
    assert "connect_notebook" in tool_names
    assert "disconnect_notebook" in tool_names
    assert "list_notebooks" in tool_names
    assert "start_kernel" in tool_names
    assert "shutdown_kernel" in tool_names
    assert "interrupt_kernel" in tool_names
    assert "get_kernel_status" in tool_names
    assert "create_cell" in tool_names
    assert "set_cell_source" in tool_names
    assert "append_source" in tool_names
    assert "get_cell" in tool_names
    assert "get_all_cells" in tool_names
    assert "delete_cell" in tool_names
    assert "move_cell" in tool_names
    assert "execute_cell" in tool_names

    # run_code should be removed
    assert "run_code" not in tool_names


@pytest.mark.asyncio
async def test_connect_and_create_cell(mcp_client: ClientSession):
    """Connect to notebook and create a cell."""
    # Connect
    result = await mcp_client.call_tool("connect_notebook", {})
    data = _parse_json(result)
    assert data["connected"] is True
    assert "notebook_id" in data

    # Create cell (returns plain text)
    result = await mcp_client.call_tool(
        "create_cell",
        {"source": "print('hello')", "cell_type": "code"},
    )
    text = _get_text(result)
    assert "Created cell:" in text
    cell_id = _extract_cell_id(text)
    assert cell_id is not None

    # Get cell (returns formatted string with source)
    result = await mcp_client.call_tool("get_cell", {"cell_id": cell_id})
    text = _get_text(result)
    assert "print('hello')" in text


@pytest.mark.asyncio
async def test_append_source(mcp_client: ClientSession):
    """Test streaming tokens into a cell."""
    # Connect
    await mcp_client.call_tool("connect_notebook", {})

    # Create empty cell
    result = await mcp_client.call_tool("create_cell", {"source": ""})
    text = _get_text(result)
    cell_id = _extract_cell_id(text)
    assert cell_id is not None

    # Stream tokens
    tokens = ["print", "(", "'hello", " ", "world", "'", ")"]
    for token in tokens:
        result = await mcp_client.call_tool(
            "append_source",
            {"cell_id": cell_id, "text": token},
        )
        data = _parse_json(result)
        assert data["appended"] is True

    # Verify final source
    result = await mcp_client.call_tool("get_cell", {"cell_id": cell_id})
    text = _get_text(result)
    assert "print('hello world')" in text


@pytest.mark.asyncio
async def test_execute_cell_basic(mcp_client: ClientSession):
    """Test basic cell execution."""
    # Connect and start kernel
    await mcp_client.call_tool("connect_notebook", {})
    await mcp_client.call_tool("start_kernel", {})

    # Create and execute cell
    result = await mcp_client.call_tool(
        "create_cell",
        {
            "source": "print('hello from kernel')",
            "and_run": True,
            "timeout_secs": 30.0,  # Give enough time for kernel warmup
        },
    )
    text = _get_text(result)

    # Should have header with cell ID and output
    assert "cell-" in text
    assert "hello from kernel" in text


@pytest.mark.asyncio
async def test_execute_cell_partial_results(mcp_client: ClientSession):
    """Execute long-running code, verify partial results returned."""
    # Connect and start kernel
    await mcp_client.call_tool("connect_notebook", {})
    await mcp_client.call_tool("start_kernel", {})

    # Create cell with slow code - print immediately, then sleep
    result = await mcp_client.call_tool(
        "create_cell",
        {
            "source": "import time; print('start'); time.sleep(10); print('done')",
            "and_run": True,
            "timeout_secs": 2.0,  # Short timeout to test partial results
        },
    )
    text = _get_text(result)

    # Should have partial output with "start" and running status
    assert "running" in text
    assert "start" in text


@pytest.mark.asyncio
async def test_poll_for_outputs(mcp_client: ClientSession):
    """Create cell, execute, poll get_cell for updated outputs."""
    # Connect and start kernel
    await mcp_client.call_tool("connect_notebook", {})
    await mcp_client.call_tool("start_kernel", {})

    # Create cell with short delay
    result = await mcp_client.call_tool(
        "create_cell",
        {
            "source": "import time; time.sleep(1); print('done')",
            "and_run": True,
            "timeout_secs": 0.5,  # Return before completion
        },
    )
    text = _get_text(result)
    cell_id = _extract_cell_id(text)
    assert cell_id is not None

    # Should be incomplete (running status)
    assert "running" in text

    # Wait and poll
    await anyio.sleep(2)
    result = await mcp_client.call_tool("get_cell", {"cell_id": cell_id})
    text = _get_text(result)

    # Should now have the output
    assert "done" in text


@pytest.mark.asyncio
async def test_output_ordering(mcp_client: ClientSession):
    """Verify interleaved outputs maintain order."""
    # Connect and start kernel
    await mcp_client.call_tool("connect_notebook", {})
    await mcp_client.call_tool("start_kernel", {})

    # Code that produces interleaved outputs
    code = """
import sys
print('a', flush=True)
from IPython.display import display
display('b')
print('c', flush=True)
"""
    result = await mcp_client.call_tool(
        "create_cell",
        {
            "source": code,
            "and_run": True,
            "timeout_secs": 30.0,
        },
    )
    text = _get_text(result)

    # Should contain a, b, c in that order
    assert "a" in text, "Output should contain 'a'"
    assert "b" in text, "Output should contain 'b'"
    assert "c" in text, "Output should contain 'c'"

    # Verify ordering: a comes before b, b comes before c
    pos_a = text.find("\na\n")  # Look for 'a' on its own line
    pos_b = text.find("'b'")  # display('b') shows as 'b'
    pos_c = text.find("\nc\n")  # Look for 'c' on its own line
    assert pos_a < pos_b < pos_c, (
        f"Outputs should be in order a, b, c. Got positions: a={pos_a}, b={pos_b}, c={pos_c}"
    )


@pytest.mark.asyncio
async def test_get_kernel_status(mcp_client: ClientSession):
    """Test kernel status reporting."""
    # Connect
    await mcp_client.call_tool("connect_notebook", {})

    # Check status before starting kernel
    result = await mcp_client.call_tool("get_kernel_status", {})
    data = _parse_json(result)
    assert data.get("kernel_started") is False

    # Start kernel
    await mcp_client.call_tool("start_kernel", {})

    # Check status after starting
    result = await mcp_client.call_tool("get_kernel_status", {})
    data = _parse_json(result)
    assert data.get("kernel_started") is True


@pytest.mark.asyncio
async def test_delete_cell(mcp_client: ClientSession):
    """Test cell deletion."""
    # Connect
    await mcp_client.call_tool("connect_notebook", {})

    # Create cell
    result = await mcp_client.call_tool("create_cell", {"source": "x = 1"})
    text = _get_text(result)
    cell_id = _extract_cell_id(text)
    assert cell_id is not None

    # Delete cell
    result = await mcp_client.call_tool("delete_cell", {"cell_id": cell_id})
    data = _parse_json(result)
    assert data["deleted"] is True

    # Verify it's gone - get_all_cells returns formatted string
    result = await mcp_client.call_tool("get_all_cells", {})
    text = _get_text(result)
    # The full cell_id shouldn't appear (we only show first 8 chars in header)
    assert cell_id not in text


@pytest.mark.asyncio
async def test_move_cell(mcp_client: ClientSession):
    """Test cell reordering."""
    await mcp_client.call_tool("connect_notebook", {})

    result = await mcp_client.call_tool("create_cell", {"source": "first"})
    first_id = _extract_cell_id(_get_text(result))
    assert first_id is not None

    result = await mcp_client.call_tool("create_cell", {"source": "second"})
    second_id = _extract_cell_id(_get_text(result))
    assert second_id is not None

    result = await mcp_client.call_tool("create_cell", {"source": "third"})
    third_id = _extract_cell_id(_get_text(result))
    assert third_id is not None

    result = await mcp_client.call_tool(
        "move_cell",
        {"cell_id": third_id, "after_cell_id": first_id},
    )
    data = _parse_json(result)
    assert data["moved"] is True
    assert data["cell_id"] == third_id
    assert data["after_cell_id"] == first_id

    result = await mcp_client.call_tool("get_all_cells", {})
    text = _get_text(result)
    assert text.index("first") < text.index("third") < text.index("second")

    result = await mcp_client.call_tool(
        "move_cell",
        {"cell_id": second_id, "after_cell_id": None},
    )
    data = _parse_json(result)
    assert data["moved"] is True
    assert data["cell_id"] == second_id
    assert data["after_cell_id"] is None

    result = await mcp_client.call_tool("get_all_cells", {})
    text = _get_text(result)
    assert text.index("second") < text.index("first") < text.index("third")
