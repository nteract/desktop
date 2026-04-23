#!/usr/bin/env python3
"""Measure CRDT sync correctness across multiple concurrent peers.

Extends the chaos-gremlin pattern: N peers connect to the same notebook,
perform random mutations concurrently, then verify convergence — all peers
must agree on cell IDs, order, sources, and output counts.

Usage:
    uv run python scripts/metrics/sync-correctness.py
    uv run python scripts/metrics/sync-correctness.py --gremlins 3 --rounds 30
    uv run python scripts/metrics/sync-correctness.py --seed 42 --json-only
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import random
import sys
import time

from runtimed import KERNEL_STATUS, Client

CODE_SNIPPETS = [
    "1 + 1",
    "list(range(5))",
    "import math; math.pi",
    "'hello' * 3",
    "sum(range(100))",
    "len(dir())",
    "True or False",
]

MARKDOWN_SNIPPETS = [
    "# Section",
    "## Subsection",
    "**bold** text",
    "- item one\n- item two",
]


async def run_peer(
    peer_id: int,
    notebook_id: str,
    rounds: int,
    delay: float,
    socket_path: str | None,
    log_fn,
) -> dict:
    """Run one peer's chaos actions. Returns action counts and errors."""
    name = f"Peer-{peer_id}"
    client = Client(socket_path=socket_path, peer_label=name)
    notebook = await client.join_notebook(notebook_id)

    # Wait for initial sync
    sync_deadline = time.monotonic() + 10
    while not notebook.cells.ids and time.monotonic() < sync_deadline:
        await asyncio.sleep(0.1)

    actions = 0
    errors = 0

    for i in range(rounds):
        action = random.choice(["create_code", "create_md", "edit", "delete", "execute"])
        actions += 1

        try:
            ids = notebook.cells.ids
            if action == "create_code":
                source = random.choice(CODE_SNIPPETS)
                await notebook.cells.create(source=f"# {name}\n{source}")
            elif action == "create_md":
                source = random.choice(MARKDOWN_SNIPPETS)
                await notebook.cells.create(source=source, cell_type="markdown")
            elif action == "edit" and ids:
                cell = notebook.cells[random.choice(ids)]
                await cell.append(f"\n# edited by {name}")
            elif action == "delete" and len(ids) > 1:
                await notebook.cells[random.choice(ids)].delete()
            elif action == "execute" and ids:
                cell = notebook.cells[random.choice(ids)]
                if cell.cell_type == "code":
                    with contextlib.suppress(asyncio.TimeoutError):
                        await cell.run(timeout_secs=10)
        except Exception as e:
            errors += 1
            log_fn(f"  [{name}] Error on round {i + 1}: {e}")

        await asyncio.sleep(delay + random.uniform(0, delay))

    return {"peer_id": peer_id, "actions": actions, "errors": errors, "notebook": notebook}


async def read_notebook_state(notebook) -> dict:
    """Snapshot the notebook state as seen by this peer."""
    ids = notebook.cells.ids
    cells = []
    for cell_id in ids:
        cell = notebook.cells[cell_id]
        cells.append(
            {
                "id": cell_id,
                "type": cell.cell_type,
                "source": cell.source,
                "output_count": len(cell.outputs),
            }
        )
    return {"cell_ids": ids, "cells": cells}


def compare_states(states: list[dict]) -> dict:
    """Compare notebook states across peers. Returns convergence report."""
    if len(states) < 2:
        return {"all_agree": True, "divergent_cells": 0, "details": []}

    reference = states[0]
    divergences = []

    for i, state in enumerate(states[1:], start=1):
        # Compare cell ID lists (order matters)
        if state["cell_ids"] != reference["cell_ids"]:
            ref_set = set(reference["cell_ids"])
            other_set = set(state["cell_ids"])
            divergences.append(
                {
                    "type": "cell_id_mismatch",
                    "peer_0_count": len(reference["cell_ids"]),
                    f"peer_{i}_count": len(state["cell_ids"]),
                    "only_in_peer_0": list(ref_set - other_set)[:5],
                    f"only_in_peer_{i}": list(other_set - ref_set)[:5],
                }
            )
            continue

        # Compare cell sources
        for j, (ref_cell, other_cell) in enumerate(
            zip(reference["cells"], state["cells"], strict=False)
        ):
            if ref_cell["source"] != other_cell["source"]:
                divergences.append(
                    {
                        "type": "source_mismatch",
                        "cell_id": ref_cell["id"][:12],
                        "cell_index": j,
                        "peer_0_len": len(ref_cell["source"]),
                        f"peer_{i}_len": len(other_cell["source"]),
                    }
                )

    return {
        "all_agree": len(divergences) == 0,
        "divergent_cells": len(divergences),
        "details": divergences[:10],
    }


async def measure_sync(
    socket_path: str | None = None,
    n_gremlins: int = 3,
    rounds: int = 30,
    delay: float = 0.3,
    seed: int | None = None,
    json_only: bool = False,
):
    if seed is not None:
        random.seed(seed)

    def log(msg: str) -> None:
        if not json_only:
            print(msg, file=sys.stderr)

    log(f"Sync correctness: {n_gremlins} peers, {rounds} rounds each, delay={delay}s")

    # Create the target notebook
    client = Client(socket_path=socket_path, peer_label="metric:sync-setup")
    notebook = await client.create_notebook()
    notebook_id = notebook.notebook_id

    # Seed with one cell
    await notebook.cells.create(source="# Sync correctness test\n1 + 1")

    # Try to start a kernel (optional — sync correctness doesn't require it,
    # but having one lets "execute" actions work during chaos rounds)
    kernel_ready = False
    try:
        await notebook.start()
        deadline = time.monotonic() + 30
        while notebook.runtime.kernel.status not in (KERNEL_STATUS.IDLE, KERNEL_STATUS.BUSY):
            if time.monotonic() > deadline:
                break
            await asyncio.sleep(0.2)
        kernel_ready = notebook.runtime.kernel.status in (KERNEL_STATUS.IDLE, KERNEL_STATUS.BUSY)
    except Exception as e:
        log(f"Kernel start failed (non-fatal): {e}")

    if kernel_ready:
        log(f"Notebook {notebook_id[:12]} ready with kernel. Launching peers...")
    else:
        log(f"Notebook {notebook_id[:12]} ready (no kernel). Launching peers...")
    await notebook.disconnect()

    # Run peers concurrently
    t0 = time.monotonic()
    tasks = [
        run_peer(i + 1, notebook_id, rounds, delay, socket_path, log) for i in range(n_gremlins)
    ]
    results = await asyncio.gather(*tasks, return_exceptions=True)
    chaos_elapsed = time.monotonic() - t0

    total_actions = 0
    total_errors = 0
    notebooks = []
    for r in results:
        if isinstance(r, Exception):
            log(f"  Peer FATAL: {r}")
            total_errors += 1
        else:
            total_actions += r["actions"]
            total_errors += r["errors"]
            notebooks.append(r["notebook"])

    # Wait for sync to settle
    log("Waiting for sync to settle (3s)...")
    await asyncio.sleep(3)

    # Read state from each peer
    log("Reading final state from each peer...")
    states = []
    for nb in notebooks:
        try:
            state = await read_notebook_state(nb)
            states.append(state)
        except Exception as e:
            log(f"  Failed to read state: {e}")

    # Disconnect all peers
    for nb in notebooks:
        with contextlib.suppress(Exception):
            await nb.disconnect()

    # Compare
    convergence = (
        compare_states(states)
        if len(states) >= 2
        else {
            "all_agree": None,
            "divergent_cells": 0,
            "details": ["insufficient peers for comparison"],
        }
    )

    output = {
        "metric": "sync_correctness",
        "gremlins": n_gremlins,
        "rounds": rounds,
        "total_actions": total_actions,
        "errors": total_errors,
        "chaos_elapsed_s": round(chaos_elapsed, 2),
        "peers_compared": len(states),
        "convergence": convergence,
    }

    log("")
    log(f"Actions:     {total_actions}")
    log(f"Errors:      {total_errors}")
    log(f"Elapsed:     {chaos_elapsed:.1f}s")
    log(f"Peers read:  {len(states)}")
    log(f"Converged:   {convergence['all_agree']}")
    if convergence["divergent_cells"] > 0:
        log(f"Divergences: {convergence['divergent_cells']}")
        for d in convergence["details"][:3]:
            log(f"  {d}")

    print(json.dumps(output, indent=2))


def main():
    parser = argparse.ArgumentParser(
        prog="sync-correctness",
        description="Measure CRDT sync convergence across concurrent peers",
    )
    parser.add_argument(
        "--gremlins",
        type=int,
        default=3,
        help="Number of concurrent peers (default: 3)",
    )
    parser.add_argument(
        "--rounds",
        type=int,
        default=30,
        help="Chaos rounds per peer (default: 30)",
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
    parser.add_argument(
        "--socket",
        default=None,
        help="Daemon socket path (default: auto-discover)",
    )
    parser.add_argument(
        "--json-only",
        action="store_true",
        help="Suppress human-readable output, print only JSON",
    )
    args = parser.parse_args()

    try:
        asyncio.run(
            measure_sync(
                socket_path=args.socket,
                n_gremlins=args.gremlins,
                rounds=args.rounds,
                delay=args.delay,
                seed=args.seed,
                json_only=args.json_only,
            )
        )
    except KeyboardInterrupt:
        print("\nInterrupted.", file=sys.stderr)


if __name__ == "__main__":
    main()
