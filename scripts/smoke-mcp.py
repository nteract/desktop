"""Cross-platform CI smoke test driving the runtimed daemon over MCP.

Spawns `runt mcp` (or `runt.exe mcp`) over stdio, creates an ephemeral
notebook, runs a trivial code cell, and asserts the output. Exits
non-zero on any failure.

Usage:
    python scripts/smoke-mcp.py <path-to-runt>
"""

from __future__ import annotations

import asyncio
import re
import sys
from pathlib import Path

# The MCP server's tool-result formatter uses ━ (U+2501) as a section
# separator. Windows runners default to cp1252, which can't encode that.
# Force UTF-8 on stdout/stderr so the smoke can print every response.
if sys.stdout.encoding and sys.stdout.encoding.lower() != "utf-8":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if sys.stderr.encoding and sys.stderr.encoding.lower() != "utf-8":
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

from mcp import ClientSession, StdioServerParameters  # noqa: E402
from mcp.client.stdio import stdio_client  # noqa: E402

CELL_ID_RE = re.compile(r"cell-[0-9a-f-]{36}")
EXEC_ID_RE = re.compile(r"exec=([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
DONE_RE = re.compile(r"\bdone\b", re.IGNORECASE)
ERROR_RE = re.compile(r"\berror\b", re.IGNORECASE)


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

        match = CELL_ID_RE.search(text_of(cell))
        if not match:
            fail(f"could not parse cell_id from create_cell response: {text_of(cell)}")
        cell_id = match.group(0)

        print(f"[smoke] execute_cell {cell_id}")
        exec_result = await session.call_tool("execute_cell", {"cell_id": cell_id})
        if exec_result.isError:
            fail(f"execute_cell errored: {text_of(exec_result)}")
        print(text_of(exec_result))

        match = EXEC_ID_RE.search(text_of(exec_result))
        if not match:
            fail(f"could not parse execution_id from execute_cell response: {text_of(exec_result)}")
        execution_id = match.group(1)
        print(f"[smoke] execution_id={execution_id}")

        # Two paths: execute_cell can return synchronously (small fast cell) or
        # leave us to poll get_results. Treat both uniformly by extracting the
        # actual stdout portion (after the trailing ━━━ separator from the
        # rich tool formatter).
        def stdout_of(body: str) -> str:
            parts = body.split("━━━")
            return parts[-1].strip() if len(parts) > 1 else body.strip()

        if DONE_RE.search(text_of(exec_result)):
            out = stdout_of(text_of(exec_result))
            if out == "2":
                print("[smoke] PASS - synchronous execution returned '2'")
                return
            if ERROR_RE.search(text_of(exec_result)):
                fail(f"execute_cell reported error: {text_of(exec_result)}")
            print(f"[smoke] execute_cell returned 'done' but output was {out!r}; polling anyway")

        print(f"[smoke] poll get_results({execution_id})")
        for attempt in range(60):
            await asyncio.sleep(2)
            results = await session.call_tool("get_results", {"execution_id": execution_id})
            body = text_of(results)
            if ERROR_RE.search(body) and "Execution not found" not in body:
                fail(f"execution errored: {body}")
            if DONE_RE.search(body):
                print(f"[smoke] result after {attempt * 2}s:")
                print(body)
                out = stdout_of(body)
                if out == "2":
                    print("[smoke] PASS - polled result was '2'")
                    return
                fail(f"execution finished but stdout was {out!r}, expected '2'")
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
