#!/usr/bin/env python3
"""Measure execution round-trip latency at the Python SDK level.

Captures the full round-trip: request to daemon, daemon queues cell,
kernel executes, IOPub results flow back, RuntimeStateDoc updates,
client observes completion.

Usage:
    uv run python scripts/metrics/execution-latency.py
    uv run python scripts/metrics/execution-latency.py --executions 200
    uv run python scripts/metrics/execution-latency.py --socket /path/to/sock --json-only
"""

from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import sys
import time

from runtimed import Client


async def measure_latency(
    socket_path: str | None = None,
    n_executions: int = 100,
    json_only: bool = False,
):
    def log(msg: str) -> None:
        if not json_only:
            print(msg, file=sys.stderr)

    client = Client(socket_path=socket_path, peer_label="metric:latency")

    log("Creating notebook and starting kernel...")
    notebook = await client.create_notebook()
    await notebook.start()

    # Wait for kernel to be idle (may take a while if prewarmed pool is empty)
    deadline = time.monotonic() + 120
    while notebook.runtime.kernel.status not in ("idle", "busy"):
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
    while notebook.runtime.kernel.status == "busy":
        if time.monotonic() > deadline:
            break
        await asyncio.sleep(0.1)

    cell = await notebook.cells.create(source="1+1")

    # Cold start: first execution after kernel launch
    log("Measuring cold start execution...")
    t0 = time.monotonic()
    result = await cell.run(timeout_secs=30)
    cold_start_ms = (time.monotonic() - t0) * 1000
    if not result.success:
        log(f"WARNING: Cold start execution failed: {result.error}")

    # Warm executions
    log(f"Measuring {n_executions} warm executions...")
    warm_times: list[float] = []
    failures = 0

    for i in range(n_executions):
        t0 = time.monotonic()
        try:
            result = await cell.run(timeout_secs=15)
            elapsed_ms = (time.monotonic() - t0) * 1000
            if result.success:
                warm_times.append(elapsed_ms)
            else:
                failures += 1
        except asyncio.TimeoutError:
            failures += 1

        if (i + 1) % 25 == 0:
            log(f"  {i + 1}/{n_executions} done")

    await notebook.disconnect()

    if not warm_times:
        log("ERROR: No successful executions")
        sys.exit(1)

    warm_times.sort()
    n = len(warm_times)
    output = {
        "metric": "execution_latency",
        "cold_start_ms": round(cold_start_ms, 2),
        "warm_p50_ms": round(warm_times[n // 2], 2),
        "warm_p95_ms": round(warm_times[int(n * 0.95)], 2),
        "warm_p99_ms": round(warm_times[int(n * 0.99)], 2),
        "warm_mean_ms": round(statistics.mean(warm_times), 2),
        "warm_min_ms": round(warm_times[0], 2),
        "warm_max_ms": round(warm_times[-1], 2),
        "n_executions": n_executions,
        "n_successful": n,
        "n_failed": failures,
    }

    log("")
    log(f"Cold start:  {output['cold_start_ms']:.1f}ms")
    log(f"Warm p50:    {output['warm_p50_ms']:.1f}ms")
    log(f"Warm p95:    {output['warm_p95_ms']:.1f}ms")
    log(f"Warm p99:    {output['warm_p99_ms']:.1f}ms")
    log(f"Warm range:  {output['warm_min_ms']:.1f}ms - {output['warm_max_ms']:.1f}ms")
    log(f"Failures:    {failures}/{n_executions}")

    print(json.dumps(output, indent=2))


def main():
    parser = argparse.ArgumentParser(
        prog="execution-latency",
        description="Measure cell execution round-trip latency",
    )
    parser.add_argument(
        "--executions",
        type=int,
        default=100,
        help="Number of warm executions to measure (default: 100)",
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
        measure_latency(
            socket_path=args.socket,
            n_executions=args.executions,
            json_only=args.json_only,
        )
    )


if __name__ == "__main__":
    main()
