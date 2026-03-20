"""Agent loop and CLI entrypoint for the gremlin.

The gremlin connects to a live notebook by spawning the nteract MCP server
as a subprocess. No in-process tools or runtimed imports needed — the nteract
server handles all notebook operations over stdio.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys
import time

from claude_agent_sdk import (
    AssistantMessage,
    ClaudeAgentOptions,
    McpServerConfig,
    ResultMessage,
    SystemMessage,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
    UserMessage,
    query,
)

log = logging.getLogger("gremlin")

NTERACT_SERVER_NAME = "nteract"

SYSTEM_PROMPT = """\
You are a gremlin — an autonomous agent inside a live Jupyter notebook.

This is a real-time collaborative environment. The notebook you're editing is
rendered live in the nteract desktop app. Humans and other gremlins may be
reading and editing the same document at the same time. Every cell you create,
modify, or execute appears instantly for everyone.

## How to operate

1. Start by calling get_all_cells to see the current notebook state.
2. Decide what to do based on what you see. Some ideas:
   - Fix broken code cells (read the error output, patch the source)
   - Add missing imports or documentation
   - Execute cells that haven't been run
   - Clean up empty or redundant cells
   - Improve code quality (better variable names, add type hints, simplify)
   - Create new cells that build on existing work
   - Add visualizations for data that's been computed but not plotted
   - Reorganize cells into a logical flow
3. Use surgical edits (replace_match, replace_regex) for small fixes —
   don't rewrite entire cells when a targeted edit will do.
4. After making changes, execute cells to verify they work.
5. If something breaks, fix it. If you can't fix it, move on.

## Concurrency rules

- Cells may change between when you read them and when you edit them.
  If a replace_match fails (0 matches), re-read the cell and retry.
- Other agents may create or delete cells. If a cell_id doesn't exist
  anymore, skip it and move on.
- Don't be precious about your work — someone else might edit or delete
  what you just created. That's fine.

## Style

- Keep markdown cells short. Use them for section headers or brief notes.
- When writing code, prefer clarity over cleverness.
- If the notebook has a theme or direction, follow it. If it's blank,
  pick something interesting and start building.
- When you create something, execute it so everyone can see the result.

## What NOT to do

- Don't delete other people's work unless it's clearly broken/empty.
- Don't add cells that just print "Hello World" or other filler.
- Don't spend turns asking questions — just act.
- Don't retry the same failed operation more than twice.
"""

DEFAULT_PROMPT = """\
You are joining a live notebook session. Look at what's there and respond to it.

Call get_all_cells to see the current state, then decide what to do. You might:
- Fix errors you find in existing cells
- Execute cells that haven't been run yet
- Improve or extend existing code
- Add something new that complements what's already there
- Clean up the notebook structure

If the notebook is empty, create something interesting — a small data analysis,
a visualization, or a computational exploration. Make it self-contained.

Work through the notebook methodically. Read, act, verify.
"""


def _find_socket_path() -> str:
    """Resolve the runtimed daemon socket path."""
    # Explicit env var takes priority
    path = os.environ.get("RUNTIMED_SOCKET_PATH")
    if path:
        return path

    # Try auto-discovery via the runt CLI
    import subprocess

    try:
        result = subprocess.run(
            ["runt", "daemon", "status", "--json"],
            capture_output=True,
            text=True,
            timeout=5,
            env={**os.environ, "RUNTIMED_DEV": "1"},
        )
        if result.returncode == 0:
            import json

            info = json.loads(result.stdout)
            return info["socket_path"]
    except (FileNotFoundError, subprocess.TimeoutExpired, KeyError, json.JSONDecodeError):
        pass

    # Last resort: try the debug binary in the repo
    try:
        result = subprocess.run(
            ["./target/debug/runt", "daemon", "status", "--json"],
            capture_output=True,
            text=True,
            timeout=5,
            env={**os.environ, "RUNTIMED_DEV": "1"},
        )
        if result.returncode == 0:
            import json

            info = json.loads(result.stdout)
            return info["socket_path"]
    except (FileNotFoundError, subprocess.TimeoutExpired, KeyError, json.JSONDecodeError):
        pass

    raise RuntimeError(
        "Cannot find runtimed socket. Set RUNTIMED_SOCKET_PATH or ensure `runt daemon` is running."
    )


def _find_workspace_root() -> str:
    """Walk up from this file to find the repo root (contains Cargo.toml + pyproject.toml)."""
    d = os.path.dirname(os.path.abspath(__file__))
    for _ in range(10):
        if os.path.isfile(os.path.join(d, "Cargo.toml")) and os.path.isfile(
            os.path.join(d, "pyproject.toml")
        ):
            return d
        d = os.path.dirname(d)
    raise RuntimeError("Cannot find workspace root (looked for Cargo.toml + pyproject.toml)")


def _build_nteract_server_config(socket_path: str) -> McpServerConfig:
    """Build an McpStdioServerConfig for the nteract MCP server."""
    workspace_root = _find_workspace_root()
    return {
        "type": "stdio",
        "command": "uv",
        "args": ["run", "--no-sync", "--directory", workspace_root, "nteract"],
        "env": {
            "RUNTIMED_SOCKET_PATH": socket_path,
            "RUNTIMED_DEV": "1",
        },
    }


async def run_agent(
    notebook_id: str,
    prompt: str = DEFAULT_PROMPT,
    max_turns: int = 25,
    socket_path: str | None = None,
) -> None:
    """Spawn the nteract MCP server and run the agent loop."""
    if socket_path is None:
        socket_path = _find_socket_path()

    log.info("Daemon socket: %s", socket_path)
    log.info("Target notebook: %s", notebook_id)

    # Inject notebook ID into the prompt so the agent knows which one to use
    full_prompt = (
        f"The notebook you should work on has ID: {notebook_id}\n"
        f"Join it first with join_notebook, then proceed.\n\n"
        f"{prompt}"
    )

    nteract_config = _build_nteract_server_config(socket_path)

    opts = ClaudeAgentOptions(
        system_prompt=SYSTEM_PROMPT,
        mcp_servers={NTERACT_SERVER_NAME: nteract_config},
        permission_mode="bypassPermissions",
        max_turns=max_turns,
    )

    log.info("Starting agent loop (max_turns=%d)", max_turns)
    t0 = time.monotonic()

    turn = 0
    async for msg in query(prompt=full_prompt, options=opts):
        if isinstance(msg, SystemMessage):
            if msg.subtype == "init":
                model = msg.data.get("model", "unknown")
                source = msg.data.get("apiKeySource", "unknown")
                auth = "Claude Max" if source == "none" else f"API ({source})"
                log.info("Agent init: model=%s auth=%s", model, auth)
            else:
                log.debug("System: subtype=%s", msg.subtype)

        elif isinstance(msg, AssistantMessage):
            turn += 1
            for block in msg.content:
                if isinstance(block, TextBlock):
                    log.info("[turn %d] 💬 %s", turn, block.text[:300])
                elif isinstance(block, ThinkingBlock):
                    log.debug("[turn %d] 🧠 (thinking %d chars)", turn, len(block.thinking))
                elif isinstance(block, ToolUseBlock):
                    args_summary = {}
                    for k, v in block.input.items():
                        sv = str(v)
                        args_summary[k] = sv[:120] + "…" if len(sv) > 120 else sv
                    log.info("[turn %d] 🔧 %s(%s)", turn, block.name, args_summary)
                elif isinstance(block, ToolResultBlock):
                    content = block.content
                    if isinstance(content, str):
                        preview = content[:200]
                    elif isinstance(content, list):
                        texts = [c.get("text", "") for c in content if isinstance(c, dict)]
                        preview = " ".join(texts)[:200]
                    else:
                        preview = str(content)[:200]
                    error_tag = " ❌" if block.is_error else ""
                    log.info("[turn %d]   ← %s%s", turn, preview, error_tag)

        elif isinstance(msg, UserMessage):
            content = msg.content
            if isinstance(content, str):
                log.debug("[turn %d] 📥 user: %s", turn, content[:200])
            elif isinstance(content, list):
                for block in content:
                    if isinstance(block, ToolResultBlock):
                        preview = str(block.content)[:200] if block.content else "(empty)"
                        error_tag = " ❌" if block.is_error else ""
                        log.info("[turn %d]   ← %s%s", turn, preview, error_tag)

        elif isinstance(msg, ResultMessage):
            elapsed = time.monotonic() - t0
            log.info(
                "Finished: turns=%d cost=$%.4f elapsed=%.1fs",
                msg.num_turns,
                msg.total_cost_usd,
                elapsed,
            )
            if msg.result:
                log.info("Result: %s", msg.result[:300])
            print(f"\n{'=' * 60}")
            print(f"Done: {msg.num_turns} turns, ${msg.total_cost_usd:.4f} ({elapsed:.1f}s)")
            print(f"{'=' * 60}")
            if msg.result:
                print(msg.result)

        else:
            log.debug("Unknown message: %s", type(msg).__name__)


def main() -> None:
    """CLI entrypoint."""
    parser = argparse.ArgumentParser(
        prog="gremlin",
        description="Autonomous notebook agent — reads, reacts, edits live",
    )
    parser.add_argument("notebook_id", help="Notebook ID to connect to")
    parser.add_argument(
        "prompt",
        nargs="?",
        default=DEFAULT_PROMPT,
        help="Custom prompt (default: autonomous mode)",
    )
    parser.add_argument("--max-turns", type=int, default=25, help="Max agent turns")
    parser.add_argument(
        "--socket",
        default=None,
        help="Daemon socket path (default: auto-discover)",
    )
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
        asyncio.run(
            run_agent(
                args.notebook_id,
                args.prompt,
                args.max_turns,
                socket_path=args.socket,
            )
        )
    except KeyboardInterrupt:
        log.info("Interrupted by user")
    except Exception:
        log.exception("Fatal error")
        sys.exit(1)
