"""Cross-platform CI smoke test driving the runtimed daemon over MCP.

Spawns `runt mcp` (or `runt.exe mcp`) over stdio and runs two passes:

  1. Basic: ephemeral notebook, `print(1+1)`, assert stdout == "2".
     Proves install + daemon + IPC + kernel-launch wire up end-to-end.
  2. Polars: ephemeral notebook with `dependencies=["polars"]`, build a
     small DataFrame, render it as the cell's execute_result, assert the
     rendered output contains the expected column names and values.
     Proves uv env resolution + package install + dataframe display.

Exits non-zero on any failure.

Usage:
    python scripts/smoke-mcp.py <path-to-runt>
"""

from __future__ import annotations

import asyncio
import json
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
TRANSIENT_KERNEL_LAUNCH_RE = re.compile(
    r"Text file busy \(os error 26\)|Kernel launch timed out or failed",
    re.IGNORECASE,
)
MAX_PASS_ATTEMPTS = 3


class SmokeRetry(Exception):
    """Raised when a smoke pass hit a transient condition worth retrying."""


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def text_of(result) -> str:
    """Concat all text-typed content blocks from a tool result."""
    return "\n".join(c.text for c in result.content if hasattr(c, "text"))


def stdout_of(body: str) -> str:
    """Strip the rich tool formatter's ━━━ banner so we get the kernel output.

    `execute_cell` and `get_results` both return human-formatted text where
    the actual stdout / execute_result body sits after the trailing ━━━
    separator. Take the last segment, or the whole body if no banner.
    """
    parts = body.split("━━━")
    return parts[-1].strip() if len(parts) > 1 else body.strip()


def parse_json_body(body: str) -> dict | None:
    """Best-effort parse for tool responses that are printed as JSON."""
    text = body.strip()
    if not text:
        return None

    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        start = text.find("{")
        end = text.rfind("}")
        if start == -1 or end == -1 or end <= start:
            return None
        try:
            parsed = json.loads(text[start : end + 1])
        except json.JSONDecodeError:
            return None

    return parsed if isinstance(parsed, dict) else None


def kernel_launch_error(body: str) -> str | None:
    """Return create_notebook kernel launch details when the runtime failed."""
    parsed = parse_json_body(body)
    if not parsed:
        return None

    runtime = parsed.get("runtime")
    if not isinstance(runtime, dict) or runtime.get("kernel_status") != "error":
        return None

    details = runtime.get("error_details")
    return str(details) if details else "kernel launch failed"


async def create_notebook_checked(
    session: ClientSession,
    label: str,
    args: dict | None = None,
) -> None:
    """Create a notebook and fail/retry immediately if auto-launch failed."""
    create = await session.call_tool("create_notebook", args or {})
    body = text_of(create)
    if create.isError:
        fail(f"create_notebook({label}) errored: {body}")
    print(body)

    error_details = kernel_launch_error(body)
    if not error_details:
        return

    message = f"create_notebook({label}) reported kernel launch error: {error_details}"
    if TRANSIENT_KERNEL_LAUNCH_RE.search(error_details):
        raise SmokeRetry(message)
    fail(message)


async def run_smoke_pass(label: str, pass_fn, session: ClientSession) -> None:
    """Run a smoke pass with bounded retries for known launch flakes."""
    for attempt in range(1, MAX_PASS_ATTEMPTS + 1):
        try:
            await pass_fn(session)
            return
        except SmokeRetry as exc:
            if attempt == MAX_PASS_ATTEMPTS:
                fail(f"[{label}] exhausted retries after transient failure: {exc}")

            delay = attempt * 2
            print(f"[{label}] transient failure on attempt {attempt}: {exc}", file=sys.stderr)
            print(f"[{label}] retrying in {delay}s", file=sys.stderr)
            await asyncio.sleep(delay)


async def run_cell_and_get_body(session: ClientSession, source: str, label: str) -> str:
    """Create a code cell, execute it, return the result body once `done`.

    Polls `get_results` for up to 240s to cover the slow path where the
    kernel needs to install dependencies on first execute.
    """
    print(f"[{label}] create_cell")
    cell = await session.call_tool(
        "create_cell",
        {"cell_type": "code", "source": source},
    )
    if cell.isError:
        fail(f"create_cell errored: {text_of(cell)}")
    print(text_of(cell))

    match = CELL_ID_RE.search(text_of(cell))
    if not match:
        fail(f"could not parse cell_id from create_cell response: {text_of(cell)}")
    cell_id = match.group(0)

    print(f"[{label}] execute_cell {cell_id}")
    exec_result = await session.call_tool("execute_cell", {"cell_id": cell_id})
    if exec_result.isError:
        fail(f"execute_cell errored: {text_of(exec_result)}")
    print(text_of(exec_result))

    body = text_of(exec_result)
    if DONE_RE.search(body):
        if ERROR_RE.search(body):
            fail(f"execute_cell reported error: {body}")
        return body

    match = EXEC_ID_RE.search(body)
    if not match:
        fail(f"could not parse execution_id from execute_cell response: {body}")
    execution_id = match.group(1)
    print(f"[{label}] execution_id={execution_id} - polling get_results")

    for attempt in range(120):
        await asyncio.sleep(2)
        results = await session.call_tool("get_results", {"execution_id": execution_id})
        body = text_of(results)
        if ERROR_RE.search(body) and "Execution not found" not in body:
            fail(f"execution errored: {body}")
        if DONE_RE.search(body):
            print(f"[{label}] result after {attempt * 2}s:")
            print(body)
            return body
        if attempt % 5 == 0:
            print(f"[{label}] attempt {attempt}: still pending")

    fail(f"[{label}] execution did not complete within 240s")


async def basic_pass(session: ClientSession) -> None:
    """Sanity-check pass: ephemeral notebook + `print(1+1)`."""
    print("[basic] create_notebook")
    await create_notebook_checked(session, "basic")

    body = await run_cell_and_get_body(session, "print(1 + 1)", "basic")
    out = stdout_of(body)
    if out != "2":
        fail(f"[basic] stdout was {out!r}, expected '2'")
    print("[basic] PASS")


async def polars_pass(session: ClientSession) -> None:
    """Deeper pass: install polars in a fresh uv-backed notebook, render a DataFrame."""
    print("[polars] create_notebook(dependencies=['polars'])")
    await create_notebook_checked(session, "polars", {"dependencies": ["polars"]})

    # Final expression `df` triggers an execute_result with the polars repr
    # (text/html + text/plain). Asserting on column names and values keeps the
    # test resilient to repr formatting changes (tabular characters, padding).
    src = (
        "import polars as pl\n"
        "df = pl.DataFrame({'name': ['a', 'b', 'c'], 'value': [10, 20, 30]})\n"
        "df\n"
    )
    body = await run_cell_and_get_body(session, src, "polars")
    out = stdout_of(body)

    expected_tokens = ("name", "value", "10", "20", "30")
    missing = [tok for tok in expected_tokens if tok not in out]
    if missing:
        fail(f"[polars] result missing tokens {missing!r}; rendered output was:\n{out}")
    print("[polars] PASS")


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

        await run_smoke_pass("basic", basic_pass, session)
        await run_smoke_pass("polars", polars_pass, session)
        print("[smoke] ALL PASSES GREEN")


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
