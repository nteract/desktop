"""Notebook tools for the gremlin agent.

Tools are constructed via `make_tools(session)` which captures the
`runtimed.AsyncSession` in closures — no module-level globals needed.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

from claude_agent_sdk import SdkMcpTool, tool

if TYPE_CHECKING:
    import runtimed

SERVER_NAME = "notebook"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _text(t: str) -> dict:
    return {"content": [{"type": "text", "text": t}]}


def _error(t: str) -> dict:
    return {"content": [{"type": "text", "text": f"ERROR: {t}"}], "isError": True}


# ---------------------------------------------------------------------------
# Factory
# ---------------------------------------------------------------------------


def make_tools(session: runtimed.AsyncSession) -> list[SdkMcpTool]:
    """Build all tools with *session* captured by closure.

    Returns a list of `SdkMcpTool` instances ready for
    `create_sdk_mcp_server`.
    """

    s = session  # short alias used by every tool below

    # ── Reading ───────────────────────────────────────────────────────

    @tool("get_cells", "Get all cells with source previews and output summaries", {})
    async def get_cells(args: dict) -> dict:
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
                    elif o.ename:
                        out_preview = f" → ERROR: {o.ename}: {(o.evalue or '')[:60]}"
            lines.append(
                f"{i} | {c.cell_type} | {c.id} | exec={c.execution_count} "
                f"| outs={outs} | {src}{out_preview}"
            )
        return _text("\n".join(lines) if lines else "(empty notebook)")

    @tool(
        "get_cell",
        "Get a single cell's full source and outputs. Use this to read a cell "
        "before editing it or to inspect error tracebacks.",
        {"cell_id": str},
    )
    async def get_cell(args: dict) -> dict:
        try:
            cell = await s.get_cell(cell_id=args["cell_id"])
        except Exception as e:
            return _error(f"Cell not found: {e}")
        parts = [f"[{cell.cell_type}] id={cell.id} exec={cell.execution_count}"]
        if cell.source:
            parts.append(f"Source:\n{cell.source}")
        if cell.outputs:
            for o in cell.outputs[:5]:
                if o.text:
                    parts.append(f"Output ({o.output_type}):\n{o.text[:1000]}")
                elif o.ename:
                    tb = "\n".join(o.traceback[:10]) if o.traceback else ""
                    parts.append(f"Error: {o.ename}: {o.evalue}\n{tb}")
        return _text("\n\n".join(parts))

    @tool(
        "get_runtime_state",
        "Get kernel status, execution queue, and environment sync state. "
        "Useful to check if the kernel is busy before executing.",
        {},
    )
    async def get_runtime_state(args: dict) -> dict:
        rs = await s.get_runtime_state()
        return _text(
            f"kernel: status={rs.kernel.status} name={rs.kernel.name} "
            f"lang={rs.kernel.language} env={rs.kernel.env_source}\n"
            f"queue: executing={rs.queue.executing} queued={rs.queue.queued}\n"
            f"env: in_sync={rs.env.in_sync}"
        )

    @tool(
        "get_peers",
        "List other peers (humans and agents) connected to this notebook.",
        {},
    )
    async def get_peers(args: dict) -> dict:
        peers = await s.get_peers()
        if not peers:
            return _text("No other peers connected — you're alone.")
        lines = [f"  {label} (id={pid})" for pid, label in peers]
        return _text(f"{len(peers)} peer(s):\n" + "\n".join(lines))

    # ── Cell CRUD ─────────────────────────────────────────────────────

    @tool(
        "create_cell",
        "Create a new cell. cell_type is 'code' or 'markdown'. "
        "index is the position (0 = first); omit to append at end.",
        {"source": str, "cell_type": str, "index": int},
    )
    async def create_cell(args: dict) -> dict:
        cid = await s.create_cell(
            source=args.get("source", ""),
            cell_type=args.get("cell_type", "code"),
            index=args.get("index"),
        )
        return _text(f"Created: {cid}")

    @tool("delete_cell", "Delete a cell by ID.", {"cell_id": str})
    async def delete_cell(args: dict) -> dict:
        try:
            await s.delete_cell(cell_id=args["cell_id"])
        except Exception as e:
            return _error(f"Delete failed: {e}")
        return _text(f"Deleted {args['cell_id']}")

    @tool(
        "move_cell",
        "Move a cell after another cell. Omit after_cell_id to move to the beginning.",
        {"cell_id": str, "after_cell_id": str},
    )
    async def move_cell(args: dict) -> dict:
        try:
            await s.move_cell(
                cell_id=args["cell_id"],
                after_cell_id=args.get("after_cell_id"),
            )
        except Exception as e:
            return _error(f"Move failed: {e}")
        return _text("Moved")

    # ── Editing ───────────────────────────────────────────────────────

    @tool(
        "set_source",
        "Replace a cell's entire source. Use splice_source or replace_match "
        "for smaller, targeted edits.",
        {"cell_id": str, "source": str},
    )
    async def set_source(args: dict) -> dict:
        try:
            await s.set_source(cell_id=args["cell_id"], source=args["source"])
        except Exception as e:
            return _error(f"set_source failed: {e}")
        return _text("Updated")

    @tool(
        "splice_source",
        "Character-level splice: delete `delete_count` chars at `index`, "
        "then insert `text`. This is the most precise edit operation — it "
        "merges cleanly with concurrent edits from other peers.",
        {"cell_id": str, "index": int, "delete_count": int, "text": str},
    )
    async def splice_source(args: dict) -> dict:
        try:
            await s.splice_source(
                cell_id=args["cell_id"],
                index=args["index"],
                delete_count=args.get("delete_count", 0),
                text=args.get("text", ""),
            )
        except Exception as e:
            return _error(f"splice_source failed: {e}")
        return _text(f"Spliced at offset {args['index']}")

    @tool(
        "replace_match",
        "Find and replace literal text in a cell (must match exactly once). "
        "Use context_before/context_after to disambiguate if there are "
        "multiple occurrences.",
        {
            "cell_id": str,
            "match": str,
            "content": str,
            "context_before": str,
            "context_after": str,
        },
    )
    async def replace_match(args: dict) -> dict:
        src = await s.get_cell_source(cell_id=args["cell_id"])
        if src is None:
            return _error("Cell not found or has no source")

        match_text = args["match"]
        content = args["content"]
        ctx_before = args.get("context_before", "")
        ctx_after = args.get("context_after", "")

        search = ctx_before + match_text + ctx_after
        occurrences = src.count(search)
        if occurrences == 0:
            occurrences = src.count(match_text)
            if occurrences == 0:
                return _error(f"No matches for: {match_text[:80]}")
            if occurrences > 1:
                return _error(
                    f"Found {occurrences} matches without context. "
                    "Add context_before/context_after to disambiguate."
                )
            idx = src.index(match_text)
        elif occurrences > 1:
            return _error(f"Found {occurrences} matches, need exactly 1")
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
        src = await s.get_cell_source(cell_id=args["cell_id"])
        if src is None:
            return _error("Cell not found or has no source")

        try:
            matches = list(re.finditer(args["pattern"], src, re.MULTILINE))
        except re.error as e:
            return _error(f"Invalid regex: {e}")

        if len(matches) == 0:
            return _error(f"No matches for pattern: {args['pattern'][:80]}")
        if len(matches) > 1:
            offsets = [m.start() for m in matches]
            return _error(f"Found {len(matches)} matches at offsets {offsets}, need exactly 1")

        m = matches[0]
        await s.splice_source(
            cell_id=args["cell_id"],
            index=m.start(),
            delete_count=m.end() - m.start(),
            text=args["content"],
        )
        return _text(f"Replaced span [{m.start()}:{m.end()}]")

    @tool(
        "set_cell_type",
        "Change a cell's type to 'code' or 'markdown'.",
        {"cell_id": str, "cell_type": str},
    )
    async def set_cell_type(args: dict) -> dict:
        try:
            await s.set_cell_type(cell_id=args["cell_id"], cell_type=args["cell_type"])
        except Exception as e:
            return _error(f"set_cell_type failed: {e}")
        return _text(f"Changed to {args['cell_type']}")

    # ── Execution ─────────────────────────────────────────────────────

    @tool(
        "execute_cell",
        "Execute a code cell and return outputs. If the cell takes longer than "
        "timeout_secs (default 30), returns a timeout message — the cell is "
        "still running in the kernel.",
        {"cell_id": str, "timeout_secs": float},
    )
    async def execute_cell(args: dict) -> dict:
        timeout = args.get("timeout_secs", 30.0)
        try:
            r = await s.execute_cell(cell_id=args["cell_id"], timeout_secs=timeout)
        except Exception as e:
            err = str(e)
            if "timed out" in err.lower() or "timeout" in err.lower():
                return _text(
                    f"Cell is still executing (timed out after {timeout}s). "
                    "The kernel is working on it — check back later or interrupt."
                )
            return _error(f"Execution failed: {err}")

        out_parts = []
        for o in (r.outputs or [])[:8]:
            if o.text:
                out_parts.append(o.text[:1000])
            elif o.ename:
                out_parts.append(f"{o.ename}: {o.evalue}")
        output_text = "\n".join(out_parts) if out_parts else "(no text output)"
        return _text(f"success={r.success} outputs={len(r.outputs or [])}\n{output_text}")

    @tool("run_all_cells", "Queue all code cells for execution in order.", {})
    async def run_all_cells(args: dict) -> dict:
        n = await s.run_all_cells()
        return _text(f"Queued {n} cells for execution")

    @tool("interrupt_kernel", "Interrupt the currently executing cell.", {})
    async def interrupt_kernel(args: dict) -> dict:
        await s.interrupt()
        return _text("Interrupted")

    @tool("clear_outputs", "Clear a cell's outputs.", {"cell_id": str})
    async def clear_outputs(args: dict) -> dict:
        try:
            await s.clear_outputs(cell_id=args["cell_id"])
        except Exception as e:
            return _error(f"clear_outputs failed: {e}")
        return _text("Cleared")

    # ── Cell metadata & tags ──────────────────────────────────────────

    @tool(
        "set_cell_tags",
        "Set tags on a cell (replaces existing tags). Tags are arbitrary "
        "strings visible in the notebook UI.",
        {"cell_id": str, "tags": list},
    )
    async def set_cell_tags(args: dict) -> dict:
        try:
            await s.set_cell_tags(cell_id=args["cell_id"], tags=args["tags"])
        except Exception as e:
            return _error(f"set_cell_tags failed: {e}")
        return _text(f"Tags set: {args['tags']}")

    @tool(
        "set_cell_source_hidden",
        "Hide or show a cell's source code. Useful for presentation-style "
        "notebooks where you want to show only outputs.",
        {"cell_id": str, "hidden": bool},
    )
    async def set_cell_source_hidden(args: dict) -> dict:
        try:
            await s.set_cell_source_hidden(cell_id=args["cell_id"], hidden=args["hidden"])
        except Exception as e:
            return _error(f"set_cell_source_hidden failed: {e}")
        return _text(f"Source hidden={args['hidden']}")

    @tool(
        "set_cell_outputs_hidden",
        "Hide or show a cell's outputs.",
        {"cell_id": str, "hidden": bool},
    )
    async def set_cell_outputs_hidden(args: dict) -> dict:
        try:
            await s.set_cell_outputs_hidden(cell_id=args["cell_id"], hidden=args["hidden"])
        except Exception as e:
            return _error(f"set_cell_outputs_hidden failed: {e}")
        return _text(f"Outputs hidden={args['hidden']}")

    # ── Dependencies ──────────────────────────────────────────────────

    @tool(
        "get_dependencies",
        "Get the notebook's inline UV dependencies (from notebook metadata).",
        {},
    )
    async def get_dependencies(args: dict) -> dict:
        deps = await s.get_uv_dependencies()
        if not deps:
            return _text("No inline dependencies set.")
        return _text("Dependencies:\n" + "\n".join(f"  - {d}" for d in deps))

    @tool(
        "add_dependency",
        "Add an inline UV dependency (e.g. 'pandas>=2.0'). The notebook will "
        "need an environment sync after adding dependencies.",
        {"package": str},
    )
    async def add_dependency(args: dict) -> dict:
        try:
            await s.add_uv_dependency(package=args["package"])
        except Exception as e:
            return _error(f"add_dependency failed: {e}")
        return _text(f"Added: {args['package']}")

    @tool(
        "sync_environment",
        "Sync the kernel environment with the notebook's declared dependencies. "
        "Call this after adding or removing dependencies.",
        {},
    )
    async def sync_environment(args: dict) -> dict:
        try:
            result = await s.sync_environment()
        except Exception as e:
            return _error(f"sync_environment failed: {e}")
        parts = [f"success={result.success}"]
        if result.synced_packages:
            parts.append(f"synced: {', '.join(result.synced_packages)}")
        if result.error:
            parts.append(f"error: {result.error}")
        if result.needs_restart:
            parts.append("(kernel restart needed)")
        return _text(" | ".join(parts))

    # ── Collect ───────────────────────────────────────────────────────

    return [
        # Reading
        get_cells,
        get_cell,
        get_runtime_state,
        get_peers,
        # Cell CRUD
        create_cell,
        delete_cell,
        move_cell,
        # Editing
        set_source,
        splice_source,
        replace_match,
        replace_regex,
        set_cell_type,
        # Execution
        execute_cell,
        run_all_cells,
        interrupt_kernel,
        clear_outputs,
        # Cell metadata
        set_cell_tags,
        set_cell_source_hidden,
        set_cell_outputs_hidden,
        # Dependencies
        get_dependencies,
        add_dependency,
        sync_environment,
    ]
