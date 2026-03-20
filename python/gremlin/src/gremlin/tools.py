"""Notebook tools for the gremlin agent.

Each tool wraps a runtimed.AsyncSession method and returns a dict
suitable for claude-agent-sdk tool responses.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

from claude_agent_sdk import tool

SERVER_NAME = "notebook"

if TYPE_CHECKING:
    import runtimed

# ---------------------------------------------------------------------------
# Module state — set by the agent before any tools are called
# ---------------------------------------------------------------------------

_session: runtimed.AsyncSession | None = None


def set_session(s: runtimed.AsyncSession) -> None:
    global _session
    _session = s


def get_session() -> runtimed.AsyncSession:
    if _session is None:
        raise RuntimeError("No active notebook session — call set_session() first")
    return _session


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _text(t: str) -> dict:
    return {"content": [{"type": "text", "text": t}]}


# ---------------------------------------------------------------------------
# Tools: Reading
# ---------------------------------------------------------------------------


@tool("get_cells", "Get all cells with source previews and output counts", {})
async def get_cells(args: dict) -> dict:
    s = get_session()
    cells = await s.get_cells()
    lines = []
    for i, c in enumerate(cells):
        src = (c.source or "")[:120].replace("\n", "↵")
        outs = len(c.outputs) if c.outputs else 0
        out_preview = ""
        if c.outputs:
            for o in c.outputs[:1]:
                if o.text:
                    out_preview = " → " + o.text[:80].replace("\n", "↵")
        lines.append(
            f"{i} | {c.cell_type} | {c.id} | exec={c.execution_count} "
            f"| outs={outs} | {src}{out_preview}"
        )
    return _text("\n".join(lines) if lines else "(empty notebook)")


@tool(
    "get_cell",
    "Get a single cell's full source and output text",
    {"cell_id": str},
)
async def get_cell(args: dict) -> dict:
    s = get_session()
    cell = await s.get_cell(cell_id=args["cell_id"])
    parts = [f"[{cell.cell_type}] id={cell.id} exec={cell.execution_count}"]
    if cell.source:
        parts.append(f"Source:\n{cell.source}")
    if cell.outputs:
        for o in cell.outputs[:5]:
            if o.text:
                parts.append(f"Output ({o.output_type}):\n{o.text[:500]}")
    return _text("\n\n".join(parts))


@tool(
    "get_runtime_state",
    "Get kernel status, execution queue, and environment info",
    {},
)
async def get_runtime_state(args: dict) -> dict:
    s = get_session()
    rs = await s.get_runtime_state()
    return _text(
        f"kernel: status={rs.kernel.status} name={rs.kernel.name} "
        f"lang={rs.kernel.language} env={rs.kernel.env_source}\n"
        f"queue: executing={rs.queue.executing} queued={rs.queue.queued}\n"
        f"env: in_sync={rs.env.in_sync}"
    )


# ---------------------------------------------------------------------------
# Tools: Cell CRUD
# ---------------------------------------------------------------------------


@tool(
    "create_cell",
    "Create a cell with source code or markdown",
    {"source": str, "cell_type": str, "index": int},
)
async def create_cell(args: dict) -> dict:
    s = get_session()
    cid = await s.create_cell(
        source=args.get("source", ""),
        cell_type=args.get("cell_type", "code"),
        index=args.get("index"),
    )
    return _text(f"Created: {cid}")


@tool("delete_cell", "Delete a cell from the notebook", {"cell_id": str})
async def delete_cell(args: dict) -> dict:
    await get_session().delete_cell(cell_id=args["cell_id"])
    return _text(f"Deleted {args['cell_id']}")


@tool(
    "move_cell",
    "Move a cell after another (omit after_cell_id to move to beginning)",
    {"cell_id": str, "after_cell_id": str},
)
async def move_cell(args: dict) -> dict:
    await get_session().move_cell(
        cell_id=args["cell_id"],
        after_cell_id=args.get("after_cell_id"),
    )
    return _text("Moved")


# ---------------------------------------------------------------------------
# Tools: Editing
# ---------------------------------------------------------------------------


@tool(
    "set_source",
    "Replace a cell's entire source. Prefer replace_match for small edits.",
    {"cell_id": str, "source": str},
)
async def set_source(args: dict) -> dict:
    await get_session().set_source(cell_id=args["cell_id"], source=args["source"])
    return _text("Updated")


@tool(
    "replace_match",
    "Replace literal text in a cell (must match exactly once). "
    "Use context_before/context_after to disambiguate.",
    {
        "cell_id": str,
        "match": str,
        "content": str,
        "context_before": str,
        "context_after": str,
    },
)
async def replace_match(args: dict) -> dict:
    s = get_session()
    src = await s.get_cell_source(cell_id=args["cell_id"])
    if src is None:
        return _text("Cell not found")

    match_text = args["match"]
    content = args["content"]
    ctx_before = args.get("context_before", "")
    ctx_after = args.get("context_after", "")

    # Build the search pattern with optional context
    search = ctx_before + match_text + ctx_after
    occurrences = src.count(search)
    if occurrences == 0:
        # Fall back to matching without context
        occurrences = src.count(match_text)
        if occurrences == 0:
            return _text(f"No matches found for: {match_text[:60]}")
        if occurrences > 1:
            return _text(
                f"Found {occurrences} matches without context. "
                "Add context_before/context_after to disambiguate."
            )
        idx = src.index(match_text)
    elif occurrences > 1:
        return _text(f"Found {occurrences} matches, need exactly 1")
    else:
        idx = src.index(search) + len(ctx_before)

    await s.splice_source(
        cell_id=args["cell_id"],
        index=idx,
        delete_count=len(match_text),
        text=content,
    )
    return _text(f"Replaced at offset {idx}")


@tool(
    "replace_regex",
    "Replace a regex match in a cell (must match exactly once). "
    "re.MULTILINE is enabled. Use \\Z for end-of-cell.",
    {"cell_id": str, "pattern": str, "content": str},
)
async def replace_regex(args: dict) -> dict:
    s = get_session()
    src = await s.get_cell_source(cell_id=args["cell_id"])
    if src is None:
        return _text("Cell not found")

    try:
        matches = list(re.finditer(args["pattern"], src, re.MULTILINE))
    except re.error as e:
        return _text(f"Invalid regex: {e}")

    if len(matches) == 0:
        return _text(f"No matches for pattern: {args['pattern'][:60]}")
    if len(matches) > 1:
        offsets = [m.start() for m in matches]
        return _text(f"Found {len(matches)} matches at offsets {offsets}, need exactly 1")

    m = matches[0]
    await s.splice_source(
        cell_id=args["cell_id"],
        index=m.start(),
        delete_count=m.end() - m.start(),
        text=args["content"],
    )
    return _text(f"Replaced span [{m.start()}:{m.end()}]")


# ---------------------------------------------------------------------------
# Tools: Execution
# ---------------------------------------------------------------------------


@tool(
    "execute_cell",
    "Execute a code cell. Returns outputs or a timeout message.",
    {"cell_id": str, "timeout_secs": float},
)
async def execute_cell(args: dict) -> dict:
    s = get_session()
    timeout = args.get("timeout_secs", 15.0)
    try:
        r = await s.execute_cell(cell_id=args["cell_id"], timeout_secs=timeout)
    except Exception as e:
        err = str(e)
        if "timed out" in err.lower() or "timeout" in err.lower():
            return _text(
                f"Cell is still executing (timed out after {timeout}s). "
                "The kernel is working on it — check back later or interrupt."
            )
        return _text(f"Execution error: {err}")

    out_parts = []
    for o in (r.outputs or [])[:5]:
        if o.text:
            out_parts.append(o.text[:500])
    output_text = "\n".join(out_parts) if out_parts else "(no text output)"
    return _text(f"success={r.success} outputs={len(r.outputs or [])}\n{output_text}")


@tool("run_all_cells", "Queue all code cells for execution", {})
async def run_all_cells(args: dict) -> dict:
    await get_session().run_all_cells()
    return _text("All cells queued")


@tool("interrupt_kernel", "Interrupt the currently executing cell", {})
async def interrupt_kernel(args: dict) -> dict:
    await get_session().interrupt()
    return _text("Interrupted")


@tool("clear_outputs", "Clear a cell's outputs", {"cell_id": str})
async def clear_outputs(args: dict) -> dict:
    await get_session().clear_outputs(cell_id=args["cell_id"])
    return _text("Cleared")


# ---------------------------------------------------------------------------
# Collected tool list
# ---------------------------------------------------------------------------

ALL_TOOLS = [
    get_cells,
    get_cell,
    get_runtime_state,
    create_cell,
    delete_cell,
    move_cell,
    set_source,
    replace_match,
    replace_regex,
    execute_cell,
    run_all_cells,
    interrupt_kernel,
    clear_outputs,
]
