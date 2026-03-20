"""Agent loop and CLI entrypoint for the gremlin stress tester."""

from __future__ import annotations

import argparse
import asyncio
import logging
import sys
import time

from claude_agent_sdk import (
    ClaudeAgentOptions,
    ResultMessage,
    SystemMessage,
    create_sdk_mcp_server,
    query,
)

import runtimed
from gremlin.tools import ALL_TOOLS, SERVER_NAME, set_session

log = logging.getLogger("gremlin")

SYSTEM_PROMPT = """\
You are an expert data scientist and visualization artist working in a live
Jupyter notebook. A human is watching the notebook in real-time in the nteract
desktop app — every cell you create, edit, or execute appears instantly on
their screen.

You have tools for full notebook manipulation including surgical text edits
via replace_match and replace_regex. Use these to fix typos or refine code
without rewriting entire cells.

Guidelines:
- Be decisive. Don't ask for permission — just do it.
- If execute_cell returns a timeout, the cell is still running. Tell the
  user and move on rather than retrying immediately.
- Keep markdown cells concise. Use emoji for section headers.
- When creating visualizations, use plt.show() and keep figure sizes reasonable.
- If a cell errors, use replace_match to fix it rather than deleting and
  recreating.
"""

DEFAULT_PROMPT = """\
Look at the notebook state, then create something impressive:

1. get_cells to see current state
2. Clean up any junk (empty cells, errors, gremlin leftovers)
3. Create a "## Grand Finale" markdown header
4. Create a stunning multi-panel matplotlib visualization — generative art,
   fractals, strange attractors, or mathematical beauty. Use colormaps and
   artistic styling. Keep computation light (no huge arrays).
5. Execute it
6. Create a pandas summary cell with fun stats
7. Execute that too

Use replace_match to fix any typos rather than rewriting whole cells.
"""


async def run_agent(
    notebook_id: str,
    prompt: str = DEFAULT_PROMPT,
    max_turns: int = 25,
) -> None:
    """Connect to a notebook and run the Claude agent loop."""
    log.info("Connecting to notebook %s", notebook_id)
    client = runtimed.AsyncClient()
    session = await client.join_notebook(notebook_id)
    set_session(session)
    log.info(
        "Connected (is_connected=%s, kernel_started=%s)",
        session.is_connected,
        session.kernel_started,
    )

    try:
        rs = await session.get_runtime_state()
        log.info(
            "Kernel: %s (%s)  queue: executing=%s queued=%s  env: in_sync=%s",
            rs.kernel.status,
            rs.kernel.env_source,
            rs.queue.executing,
            rs.queue.queued,
            rs.env.in_sync,
        )
    except Exception:
        log.warning("Could not read runtime state", exc_info=True)

    tool_names = [t.name for t in ALL_TOOLS]
    log.info("Registering %d tools: %s", len(tool_names), ", ".join(tool_names))

    opts = ClaudeAgentOptions(
        system_prompt=SYSTEM_PROMPT,
        mcp_servers={SERVER_NAME: create_sdk_mcp_server(SERVER_NAME, tools=ALL_TOOLS)},
        allowed_tools=[f"mcp__{SERVER_NAME}__{t.name}" for t in ALL_TOOLS],
        permission_mode="bypassPermissions",
        max_turns=max_turns,
    )

    log.info("Starting agent loop (max_turns=%d)", max_turns)
    t0 = time.monotonic()

    async for msg in query(prompt=prompt, options=opts):
        if isinstance(msg, SystemMessage) and msg.subtype == "init":
            model = msg.data.get("model", "unknown")
            source = msg.data.get("apiKeySource", "unknown")
            auth = "Claude Max" if source == "none" else f"API ({source})"
            log.info("Agent init: model=%s auth=%s", model, auth)
        elif isinstance(msg, ResultMessage):
            elapsed = time.monotonic() - t0
            log.info(
                "Agent finished: turns=%d cost=$%.4f elapsed=%.1fs",
                msg.num_turns,
                msg.total_cost_usd,
                elapsed,
            )
            if msg.result:
                log.info("Result: %s", msg.result[:200])
            print(f"\n{'=' * 60}")
            print(f"Done: {msg.num_turns} turns, ${msg.total_cost_usd:.4f} ({elapsed:.1f}s)")
            print(f"{'=' * 60}")
            if msg.result:
                print(msg.result)
        else:
            # Log other message types at debug level for tracing
            log.debug(
                "Agent message: type=%s %r", type(msg).__name__, getattr(msg, "subtype", None)
            )


def main() -> None:
    """CLI entrypoint."""
    parser = argparse.ArgumentParser(
        prog="gremlin",
        description="Claude-powered notebook stress tester",
    )
    parser.add_argument("notebook_id", help="Notebook ID to connect to")
    parser.add_argument(
        "prompt",
        nargs="?",
        default=DEFAULT_PROMPT,
        help="Prompt for the agent (default: create something impressive)",
    )
    parser.add_argument("--max-turns", type=int, default=25, help="Max agent turns")
    parser.add_argument(
        "-v",
        "--verbose",
        action="count",
        default=0,
        help="Increase verbosity (-v info, -vv debug)",
    )
    args = parser.parse_args()

    level = logging.WARNING
    if args.verbose >= 2:
        level = logging.DEBUG
    elif args.verbose >= 1:
        level = logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(name)s %(levelname)s  %(message)s",
        datefmt="%H:%M:%S",
    )

    try:
        asyncio.run(run_agent(args.notebook_id, args.prompt, args.max_turns))
    except KeyboardInterrupt:
        log.info("Interrupted by user")
    except Exception:
        log.exception("Fatal error")
        sys.exit(1)
