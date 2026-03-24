#!/usr/bin/env python3
"""Chaos gremlin — randomly mutates a live notebook to stress-test sync.

Connects to a running runtimed daemon and performs random operations:
creates cells, edits source, executes cells, deletes cells, moves cells,
clears outputs, and adds markdown. Designed to run alongside a human in
the nteract desktop app to shake out CRDT sync bugs.

Usage:
    # Auto-discover dev daemon, join the first active notebook
    uv run python scripts/chaos-gremlin.py

    # Target nightly daemon (install matching runtimed first)
    uv pip install runtimed==2.0.3a<YYYYMMDDHHMI>  # match your nightly version
    RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/runtimed.sock \
        uv run python scripts/chaos-gremlin.py

    # Target a specific notebook
    uv run python scripts/chaos-gremlin.py --notebook-id <id>

    # Launch 3 concurrent gremlins on the same notebook
    uv run python scripts/chaos-gremlin.py --gremlins 3

    # Control chaos level
    uv run python scripts/chaos-gremlin.py --rounds 50 --delay 0.5

Note:
    When targeting the nightly daemon, the runtimed Python package version
    must match the nightly daemon version. Check with `runt-nightly status`
    (look for the Version line) and install the matching alpha wheel:
        uv pip install runtimed==2.0.3a202603230219
"""

from __future__ import annotations

import argparse
import asyncio
import random
import sys
import time

from runtimed import Client

# ── Chaos actions ─────────────────────────────────────────────────────

EMOJI = ["🐸", "🔥", "💥", "🎲", "🧪", "👻", "🌪️", "⚡", "🦆", "🎯"]

CODE_SNIPPETS = [
    'print("gremlin was here 🐸")',
    "import random; random.random()",
    "2 + 2",
    "[x**2 for x in range(10)]",
    "import sys; sys.version",
    'print("".join(__import__("random").choices("abcdef", k=20)))',
    "import math; math.pi",
    "list(range(5))",
    '{"key": "value", "n": 42}',
    "sum(range(100))",
    "import datetime; str(datetime.datetime.now())",
    "len(dir())",
    "True or False",
    "type(None).__name__",
]

MARKDOWN_SNIPPETS = [
    "# 🐸 Gremlin Section",
    "## Chaos reigns here",
    "**bold** and *italic* and `code`",
    "> A gremlin once said: break everything.",
    "- item 1\n- item 2\n- item 3",
    "| col1 | col2 |\n|------|------|\n| a | b |",
    "### 🧪 Test in progress",
    "*The gremlin is watching.*",
]

BAD_CODE = [
    "this is not valid python",
    "def f(:\n    pass",
    "import nonexistent_module_xyz",
    "1/0",
    "raise RuntimeError('gremlin attack!')",
    "[][999]",
    "print(undefined_variable_xyz)",
]


# ── Action functions ──────────────────────────────────────────────────
# Each takes (notebook, gremlin_name, log) and returns nothing.
# They read cell state from notebook.cells as needed.


async def action_create_code(notebook, name, log):
    """Create a new code cell with random content."""
    snippet = random.choice(CODE_SNIPPETS)
    emoji = random.choice(EMOJI)
    source = f"# {emoji} {name}\n{snippet}"
    cell = await notebook.cells.create(source=source, cell_type="code")
    log(f"CREATE code: {snippet[:40]}... -> {cell.id[:8]}")


async def action_create_markdown(notebook, name, log):
    """Create a new markdown cell."""
    source = random.choice(MARKDOWN_SNIPPETS)
    cell = await notebook.cells.create(source=source, cell_type="markdown")
    log(f"CREATE markdown: {source[:40]}... -> {cell.id[:8]}")


async def action_create_bad_code(notebook, name, log):
    """Create a cell with intentionally bad code."""
    source = random.choice(BAD_CODE)
    cell = await notebook.cells.create(source=f"# {name} bad code\n{source}", cell_type="code")
    log(f"CREATE bad code: {source[:40]}... -> {cell.id[:8]}")


async def action_execute(notebook, name, log):
    """Execute a random cell."""
    ids = notebook.cells.ids
    if not ids:
        return
    cell_id = random.choice(ids)
    cell = notebook.cells[cell_id]
    log(f"EXECUTE {cell_id[:8]}: {cell.source[:30] if cell.source else '(empty)'}...")
    try:
        result = await cell.run(timeout_secs=10)
        log(f"  -> success={result.success} outs={len(result.outputs)}")
    except Exception as e:
        log(f"  -> error: {e}")


async def action_delete(notebook, name, log):
    """Delete a random cell (but keep at least one)."""
    ids = notebook.cells.ids
    if len(ids) <= 1:
        return
    cell_id = random.choice(ids)
    log(f"DELETE {cell_id[:8]}")
    try:
        await notebook.cells[cell_id].delete()
    except Exception as e:
        log(f"  -> error: {e}")


async def action_edit_source(notebook, name, log):
    """Edit a random cell's source by appending a comment."""
    ids = notebook.cells.ids
    if not ids:
        return
    cell_id = random.choice(ids)
    cell = notebook.cells[cell_id]
    source = cell.source
    if source is None:
        return
    emoji = random.choice(EMOJI)
    comment = f"\n# {emoji} {name} @ {time.strftime('%H:%M:%S')}"
    try:
        await cell.append(comment)
        log(f"EDIT {cell_id[:8]}: appended comment")
    except Exception as e:
        log(f"EDIT {cell_id[:8]} error: {e}")


async def action_clear_outputs(notebook, name, log):
    """Clear outputs on a random cell."""
    ids = notebook.cells.ids
    if not ids:
        return
    cell_id = random.choice(ids)
    log(f"CLEAR outputs {cell_id[:8]}")
    try:
        await notebook.cells[cell_id].clear_outputs()
    except Exception as e:
        log(f"  -> error: {e}")


async def action_move(notebook, name, log):
    """Move a random cell to a random position."""
    ids = notebook.cells.ids
    if len(ids) < 2:
        return
    cell_id = random.choice(ids)
    other_ids = [i for i in ids if i != cell_id]
    target_id = random.choice(other_ids)
    log(f"MOVE {cell_id[:8]} after {target_id[:8]}")
    try:
        target_cell = notebook.cells[target_id]
        await notebook.cells[cell_id].move_after(target_cell)
    except Exception as e:
        log(f"  -> error: {e}")


async def action_rapid_execute(notebook, name, log):
    """Rapid-fire queue the same cell multiple times (stress test)."""
    ids = notebook.cells.ids
    if not ids:
        return
    cell_id = random.choice(ids)
    count = random.randint(3, 7)
    log(f"RAPID EXECUTE {cell_id[:8]} x{count}")
    for i in range(count):
        try:
            await notebook.cells[cell_id].queue()
        except Exception as e:
            log(f"  -> rapid exec {i + 1} error: {e}")
            break


async def action_read_and_verify(notebook, name, log):
    """Read all cells and log the state (verifies sync is working)."""
    ids = notebook.cells.ids
    log(f"READ: {len(ids)} cells")
    for i, cell_id in enumerate(ids[:5]):
        cell = notebook.cells[cell_id]
        src = cell.source[:30].replace("\n", "\\n") if cell.source else "(empty)"
        log(f"  [{i}] {cell.cell_type} ec={cell.execution_count}: {src}")
    if len(ids) > 5:
        log(f"  ... and {len(ids) - 5} more")


# ── Weighted action selection ─────────────────────────────────────────

ACTIONS = [
    (action_create_code, 20),
    (action_create_markdown, 5),
    (action_create_bad_code, 5),
    (action_execute, 25),
    (action_delete, 10),
    (action_edit_source, 15),
    (action_clear_outputs, 5),
    (action_move, 5),
    (action_rapid_execute, 5),
    (action_read_and_verify, 5),
]


def pick_action():
    """Weighted random action selection."""
    actions, weights = zip(*ACTIONS, strict=True)
    return random.choices(actions, weights=weights, k=1)[0]


# ── Single gremlin coroutine ──────────────────────────────────────────


async def run_single_gremlin(
    gremlin_id: int,
    notebook_id: str,
    rounds: int,
    delay: float,
    socket_path: str | None = None,
):
    """Run one gremlin — connects independently to the daemon."""
    name = f"Gremlin-{gremlin_id}"
    prefix = f"[{name}]"

    def log(msg):
        print(f"  {prefix} {msg}")

    client = Client(socket_path=socket_path, peer_label=name)

    print(f"{prefix} Connecting to notebook {notebook_id[:8]}...")
    notebook = await client.join_notebook(notebook_id)

    # Wait for initial sync — the local doc may be empty until the daemon
    # sends the first sync message with the document structure.  Without
    # this, fast gremlins hit "cells map not found" because they try to
    # operate before the cells map exists in the local replica.
    sync_deadline = time.monotonic() + 10
    while not notebook.cells.ids and time.monotonic() < sync_deadline:
        await asyncio.sleep(0.1)

    print(f"{prefix} Connected! Cells: {len(notebook.cells.ids)}")

    errors = 0
    for i in range(rounds):
        action = pick_action()
        action_name = action.__name__.replace("action_", "").upper()
        emoji = random.choice(EMOJI)
        print(f"{prefix} [{i + 1:3d}/{rounds}] {emoji} {action_name}")

        try:
            await action(notebook, name, log)
        except Exception as e:
            log(f"💀 CRASH: {e}")
            errors += 1

        await asyncio.sleep(delay + random.uniform(0, delay))

    print(f"{prefix} Done: {rounds} rounds, {errors} errors")
    await notebook.disconnect()
    return errors


# ── Main ──────────────────────────────────────────────────────────────


async def async_main(
    socket_path: str | None = None,
    notebook_id: str | None = None,
    gremlins: int = 1,
    rounds: int = 30,
    delay: float = 0.3,
    seed: int | None = None,
):
    if seed is not None:
        random.seed(seed)

    print(f"🐸 Chaos Gremlin Pack (count={gremlins}, rounds={rounds}, delay={delay}s)")
    print(f"   Socket: {socket_path or '(auto-discover)'}")

    # Find or create the target notebook
    if notebook_id is None:
        client = Client(socket_path=socket_path)
        notebooks = await client.list_active_notebooks()
        if notebooks:
            notebook_id = notebooks[0].notebook_id
            print(f"   Joining first active notebook: {notebook_id[:8]}...")
        else:
            print("   No active notebooks — creating one...")
            notebook = await client.create_notebook()
            notebook_id = notebook.notebook_id
            # Seed it with a cell
            await notebook.cells.create(
                source='# 🐸 Chaos Gremlin Playground\nprint("Let the chaos begin!")',
                cell_type="code",
            )
            print(f"   Created: {notebook_id[:8]}")
            await notebook.disconnect()

    print(f"   Target: {notebook_id}")
    print()

    # Launch gremlins concurrently — each connects independently
    t0 = time.monotonic()

    tasks = [
        run_single_gremlin(
            gremlin_id=i + 1,
            notebook_id=notebook_id,
            rounds=rounds,
            delay=delay,
            socket_path=socket_path,
        )
        for i in range(gremlins)
    ]

    results = await asyncio.gather(*tasks, return_exceptions=True)

    elapsed = time.monotonic() - t0
    total_errors = 0
    for i, result in enumerate(results):
        if isinstance(result, Exception):
            print(f"  Gremlin-{i + 1} FATAL: {result}")
            total_errors += 1
        elif isinstance(result, int):
            total_errors += result

    print()
    print("=" * 60)
    print(f"🐸 Chaos complete: {gremlins} gremlins, {rounds} rounds each")
    print(f"   Total errors: {total_errors}")
    print(f"   Elapsed: {elapsed:.1f}s")
    print("=" * 60)

    # Final state check
    try:
        client = Client(socket_path=socket_path)
        notebook = await client.join_notebook(notebook_id)
        ids = notebook.cells.ids
        print(f"   Final cells: {len(ids)}")
        for i, cell_id in enumerate(ids):
            cell = notebook.cells[cell_id]
            src = cell.source[:40].replace("\n", "\\n") if cell.source else "(empty)"
            outs = len(cell.outputs) if cell.outputs else 0
            ec = cell.execution_count
            print(f"   [{i}] {cell.cell_type} ec={ec} outs={outs}: {src}")
        await notebook.disconnect()
    except Exception as e:
        print(f"   (failed to read final state: {e})")

    print("🐸 Gremlins out.")


def main():
    parser = argparse.ArgumentParser(
        prog="chaos-gremlin",
        description="🐸 Randomly mutates a live notebook to stress-test sync",
    )
    parser.add_argument(
        "--notebook-id",
        default=None,
        help="Notebook ID to join (default: first active or create new)",
    )
    parser.add_argument(
        "--socket",
        default=None,
        help="Daemon socket path (default: auto-discover)",
    )
    parser.add_argument(
        "--gremlins",
        type=int,
        default=1,
        help="Number of concurrent gremlins (default: 1)",
    )
    parser.add_argument(
        "--rounds",
        type=int,
        default=30,
        help="Number of chaos rounds per gremlin (default: 30)",
    )
    parser.add_argument(
        "--delay",
        type=float,
        default=0.3,
        help="Base delay between actions in seconds (default: 0.3)",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=None,
        help="Random seed for reproducibility",
    )
    args = parser.parse_args()

    try:
        asyncio.run(
            async_main(
                socket_path=args.socket,
                notebook_id=args.notebook_id,
                gremlins=args.gremlins,
                rounds=args.rounds,
                delay=args.delay,
                seed=args.seed,
            )
        )
    except KeyboardInterrupt:
        print("\n🐸 Interrupted. Gremlins retreat.")
    except Exception:
        import traceback

        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    main()
