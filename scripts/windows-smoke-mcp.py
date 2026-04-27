"""Windows CI smoke test driving the runtimed daemon over MCP.

Spawns `runt.exe mcp` over stdio, creates an ephemeral notebook, runs a
trivial code cell, and asserts the output. Exits non-zero on any failure.

Usage:
    python scripts/windows-smoke-mcp.py <path-to-runt.exe>
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def text_of(result) -> str:
    """Concat all text-typed content blocks from a tool result."""
    return "\n".join(c.text for c in result.content if hasattr(c, "text"))


async def smoke(runt_exe: Path) -> None:
    params = StdioServerParameters(command=str(runt_exe), args=["mcp"])

    async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        print("[smoke] MCP session initialized")

        tools = await session.list_tools()
        tool_names = sorted(t.name for t in tools.tools)
        print(f"[smoke] {len(tool_names)} tools available")
        for required in ("create_notebook", "create_cell", "execute_cell", "get_results"):
            if required not in tool_names:
                fail(f"required tool missing: {required}")

        print("[smoke] create_notebook")
        create = await session.call_tool("create_notebook", {})
        if create.isError:
            fail(f"create_notebook errored: {text_of(create)}")
        print(text_of(create))

        print("[smoke] create_cell (code: 1+1)")
        cell = await session.call_tool(
            "create_cell",
            {"cell_type": "code", "source": "print(1 + 1)"},
        )
        if cell.isError:
            fail(f"create_cell errored: {text_of(cell)}")
        print(text_of(cell))

        cell_id = None
        for c in cell.content:
            if hasattr(c, "text"):
                try:
                    parsed = json.loads(c.text)
                    cell_id = parsed.get("cell_id") or parsed.get("id")
                    if cell_id:
                        break
                except (json.JSONDecodeError, AttributeError):
                    continue
        if not cell_id and cell.structuredContent:
            cell_id = cell.structuredContent.get("cell_id") or cell.structuredContent.get("id")
        if not cell_id:
            print("[smoke] WARN: could not parse cell_id from response, trying get_all_cells")
            allcells = await session.call_tool("get_all_cells", {"format": "json"})
            print(text_of(allcells))
            fail("no cell_id available to execute")

        print(f"[smoke] execute_cell {cell_id}")
        exec_result = await session.call_tool("execute_cell", {"cell_id": cell_id})
        if exec_result.isError:
            fail(f"execute_cell errored: {text_of(exec_result)}")
        print(text_of(exec_result))

        execution_id = None
        for c in exec_result.content:
            if hasattr(c, "text"):
                try:
                    parsed = json.loads(c.text)
                    execution_id = parsed.get("execution_id")
                    if execution_id:
                        break
                except (json.JSONDecodeError, AttributeError):
                    continue
        if not execution_id and exec_result.structuredContent:
            execution_id = exec_result.structuredContent.get("execution_id")
        if not execution_id:
            fail("no execution_id from execute_cell")

        print(f"[smoke] poll get_results({execution_id})")
        for attempt in range(60):
            await asyncio.sleep(2)
            results = await session.call_tool("get_results", {"execution_id": execution_id})
            body = text_of(results)
            if "done" in body.lower() or "2" in body:
                print(f"[smoke] result after {attempt * 2}s:")
                print(body)
                if "2" in body:
                    print("[smoke] PASS - got expected output 2")
                    return
                fail(f"execution finished but output didn't contain '2': {body}")
            if "error" in body.lower():
                fail(f"execution errored: {body}")
            print(f"[smoke] attempt {attempt}: still pending")

        fail("execution did not complete within 120s")


def main() -> None:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    runt_exe = Path(sys.argv[1])
    if not runt_exe.exists():
        fail(f"runt exe not found: {runt_exe}")
    asyncio.run(smoke(runt_exe))


if __name__ == "__main__":
    main()
