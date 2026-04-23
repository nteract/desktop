#!/usr/bin/env python3
"""Measure kernel execution reliability over many diverse cell executions.

Runs a battery of cells (fast expressions, imports, slow computations,
intentional errors) and measures success rate, timeout rate, and zombie
kernel detection.

Usage:
    uv run python scripts/metrics/kernel-reliability.py
    uv run python scripts/metrics/kernel-reliability.py --rounds 100
    uv run python scripts/metrics/kernel-reliability.py --socket /path/to/sock --json-only
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import statistics
import sys
import time

from runtimed import KERNEL_STATUS, Client

# Cell battery: (source, expected_success, timeout_secs)
CELL_BATTERY = [
    # Fast expressions
    ("1 + 1", True, 10),
    ("True", True, 10),
    ("len([])", True, 10),
    ("list(range(10))", True, 10),
    ("'hello' * 3", True, 10),
    # Imports
    ("import sys; sys.version", True, 15),
    ("import math; math.pi", True, 15),
    ("import json; json.dumps({'a': 1})", True, 15),
    ("import os; os.getpid()", True, 15),
    # Slightly heavier
    ("sum(range(10000))", True, 15),
    ("[x**2 for x in range(100)]", True, 15),
    ("import hashlib; hashlib.sha256(b'test').hexdigest()", True, 15),
    # Print output
    ('print("hello world")', True, 10),
    ('print("line1\\nline2\\nline3")', True, 10),
    # Intentional errors (should complete with success=False)
    ("1/0", False, 10),
    ("raise ValueError('test error')", False, 10),
    ("undefined_variable_xyz", False, 10),
    ("import nonexistent_module_abc", False, 10),
]


async def measure_reliability(
    socket_path: str | None = None,
    rounds: int = 50,
    json_only: bool = False,
):
    def log(msg: str) -> None:
        if not json_only:
            print(msg, file=sys.stderr)

    client = Client(socket_path=socket_path, peer_label="metric:reliability")

    log("Creating notebook and starting kernel...")
    notebook = await client.create_notebook()
    await notebook.start()

    # Wait for kernel idle (may take a while if prewarmed pool is empty)
    deadline = time.monotonic() + 120
    while notebook.runtime.kernel.status not in (KERNEL_STATUS.IDLE, KERNEL_STATUS.BUSY):
        if time.monotonic() > deadline:
            status = notebook.runtime.kernel.status
            print(
                f"ERROR: Kernel stuck in '{status}' after 120s",
                file=sys.stderr,
            )
            await notebook.disconnect()
            sys.exit(1)
        await asyncio.sleep(0.2)
    # If busy, wait for it to become idle
    while notebook.runtime.kernel.status == KERNEL_STATUS.BUSY:
        if time.monotonic() > deadline:
            break
        await asyncio.sleep(0.1)

    log(f"Running {rounds} rounds of {len(CELL_BATTERY)} cell types...")

    total = 0
    completed = 0
    timed_out = 0
    zombie = 0
    wrong_outcome = 0
    durations_ms: list[float] = []

    for round_num in range(rounds):
        # Pick a cell from the battery (cycle through)
        source, expect_success, timeout = CELL_BATTERY[round_num % len(CELL_BATTERY)]

        cell = await notebook.cells.create(source=source)
        total += 1

        t0 = time.monotonic()
        try:
            result = await cell.run(timeout_secs=timeout)
            elapsed_ms = (time.monotonic() - t0) * 1000
            durations_ms.append(elapsed_ms)

            if result.success != expect_success:
                wrong_outcome += 1

            completed += 1
        except asyncio.TimeoutError:
            timed_out += 1

            # Check for zombie: kernel stuck busy after timeout
            await asyncio.sleep(1)
            kernel_status = notebook.runtime.kernel.status
            if kernel_status == KERNEL_STATUS.BUSY:
                # Give it more time before declaring zombie
                await asyncio.sleep(5)
                if notebook.runtime.kernel.status == KERNEL_STATUS.BUSY:
                    zombie += 1
                    log(f"  ZOMBIE detected at round {round_num + 1}")
                    # Interrupt to recover
                    try:
                        await notebook.interrupt()
                        await asyncio.sleep(2)
                    except Exception:
                        pass
        except Exception as e:
            log(f"  Round {round_num + 1} unexpected error: {e}")

        # Clean up: delete the cell to avoid doc bloat
        with contextlib.suppress(Exception):
            await cell.delete()

        if (round_num + 1) % 25 == 0:
            log(f"  {round_num + 1}/{rounds} done")

    await notebook.disconnect()

    reliability_pct = (completed / total * 100) if total > 0 else 0

    output: dict = {
        "metric": "kernel_reliability",
        "total": total,
        "completed": completed,
        "timed_out": timed_out,
        "zombie": zombie,
        "wrong_outcome": wrong_outcome,
        "reliability_pct": round(reliability_pct, 2),
    }

    if durations_ms:
        durations_ms.sort()
        n = len(durations_ms)
        output["p50_ms"] = round(durations_ms[n // 2], 2)
        output["p95_ms"] = round(durations_ms[int(n * 0.95)], 2)
        output["mean_ms"] = round(statistics.mean(durations_ms), 2)
    else:
        output["p50_ms"] = None
        output["p95_ms"] = None
        output["mean_ms"] = None

    log("")
    log(f"Reliability: {reliability_pct:.1f}% ({completed}/{total} completed)")
    log(f"Timed out:   {timed_out}")
    log(f"Zombie:      {zombie}")
    log(f"Wrong result:{wrong_outcome}")
    if durations_ms:
        log(f"Latency p50: {output['p50_ms']:.1f}ms")
        log(f"Latency p95: {output['p95_ms']:.1f}ms")

    print(json.dumps(output, indent=2))


def main():
    parser = argparse.ArgumentParser(
        prog="kernel-reliability",
        description="Measure kernel execution reliability over diverse cell types",
    )
    parser.add_argument(
        "--rounds",
        type=int,
        default=50,
        help="Number of cell executions (default: 50)",
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

    asyncio.run(
        measure_reliability(
            socket_path=args.socket,
            rounds=args.rounds,
            json_only=args.json_only,
        )
    )


if __name__ == "__main__":
    main()
