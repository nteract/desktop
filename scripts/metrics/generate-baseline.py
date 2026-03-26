#!/usr/bin/env python3
"""Generate a metrics baseline by running all metric scripts.

Run this on main to establish the baseline that worktrees compare against.
The baseline is committed to the repo so every worktree has it.

Usage:
    uv run python scripts/metrics/generate-baseline.py
    uv run python scripts/metrics/generate-baseline.py --latency-executions 50
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

METRICS_DIR = Path(__file__).parent
BASELINE_PATH = METRICS_DIR / "baseline.json"


def run_metric(script: str, args: list[str]) -> dict | None:
    """Run a metric script and parse its JSON output."""
    cmd = [sys.executable, str(METRICS_DIR / script), "--json-only", *args]
    print(f"  Running {script}...", file=sys.stderr)
    t0 = time.monotonic()
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=600, env=os.environ)
    elapsed = time.monotonic() - t0

    if result.returncode != 0:
        stderr_msg = (
            result.stderr.strip()[-200:] if result.stderr else "kernel may not have started"
        )
        print(f"  FAILED ({elapsed:.1f}s): {stderr_msg}", file=sys.stderr)
        return None

    try:
        data = json.loads(result.stdout)
        print(f"  OK ({elapsed:.1f}s)", file=sys.stderr)
        return data
    except json.JSONDecodeError:
        print("  FAILED: invalid JSON output", file=sys.stderr)
        return None


def main():
    parser = argparse.ArgumentParser(
        prog="generate-baseline",
        description="Generate metrics baseline from all metric scripts",
    )
    parser.add_argument(
        "--latency-executions",
        type=int,
        default=50,
        help="Number of executions for latency metric (default: 50)",
    )
    parser.add_argument(
        "--reliability-rounds",
        type=int,
        default=36,
        help="Number of rounds for reliability metric (default: 36, 2 full battery cycles)",
    )
    parser.add_argument(
        "--sync-gremlins",
        type=int,
        default=3,
        help="Number of peers for sync metric (default: 3)",
    )
    parser.add_argument(
        "--sync-rounds",
        type=int,
        default=20,
        help="Rounds per peer for sync metric (default: 20)",
    )
    parser.add_argument(
        "--socket",
        default=None,
        help="Daemon socket path (default: auto-discover)",
    )
    args = parser.parse_args()

    socket_args = ["--socket", args.socket] if args.socket else []

    print("Generating metrics baseline...", file=sys.stderr)
    print(f"  Output: {BASELINE_PATH}", file=sys.stderr)
    print("", file=sys.stderr)

    # Get git info for provenance
    git_ref = (
        subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
        ).stdout.strip()
        or "unknown"
    )
    git_branch = (
        subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True,
            text=True,
        ).stdout.strip()
        or "unknown"
    )

    baseline = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "git_ref": git_ref,
        "git_branch": git_branch,
        "metrics": {},
    }

    # Run each metric (small delay between runs to let daemon reclaim resources)
    latency = run_metric(
        "execution-latency.py",
        ["--executions", str(args.latency_executions), *socket_args],
    )
    if latency:
        baseline["metrics"]["execution_latency"] = latency

    reliability = run_metric(
        "kernel-reliability.py",
        ["--rounds", str(args.reliability_rounds), *socket_args],
    )
    if reliability:
        baseline["metrics"]["kernel_reliability"] = reliability

    sync = run_metric(
        "sync-correctness.py",
        [
            "--gremlins",
            str(args.sync_gremlins),
            "--rounds",
            str(args.sync_rounds),
            "--seed",
            "42",
            *socket_args,
        ],
    )
    if sync:
        baseline["metrics"]["sync_correctness"] = sync

    # Write baseline
    n_metrics = len(baseline["metrics"])
    if n_metrics == 0:
        print("\nERROR: No metrics succeeded. Baseline not written.", file=sys.stderr)
        sys.exit(1)

    BASELINE_PATH.write_text(json.dumps(baseline, indent=2) + "\n")
    print(f"\nBaseline written: {n_metrics}/3 metrics at {git_ref} ({git_branch})", file=sys.stderr)
    print(f"  {BASELINE_PATH}", file=sys.stderr)


if __name__ == "__main__":
    main()
