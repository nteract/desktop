#!/usr/bin/env python3
"""Compare current metric results against the committed baseline.

Runs selected metrics and shows deltas vs baseline. Designed for use in
worktrees to answer "did my changes make things better or worse?"

Usage:
    uv run python scripts/metrics/compare.py                    # run all, compare
    uv run python scripts/metrics/compare.py --only latency     # just latency
    uv run python scripts/metrics/compare.py --only reliability  # just reliability
    uv run python scripts/metrics/compare.py --only sync         # just sync
    uv run python scripts/metrics/compare.py --from results.json # compare saved results
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

# Which keys to compare for each metric, and whether higher or lower is better
COMPARISONS: dict[str, list[tuple[str, str, str]]] = {
    # (key, direction, label)
    "execution_latency": [
        ("cold_start_ms", "lower", "Cold start"),
        ("warm_p50_ms", "lower", "Warm p50"),
        ("warm_p95_ms", "lower", "Warm p95"),
        ("n_failed", "lower", "Failures"),
    ],
    "kernel_reliability": [
        ("reliability_pct", "higher", "Reliability"),
        ("timed_out", "lower", "Timeouts"),
        ("zombie", "lower", "Zombies"),
        ("wrong_outcome", "lower", "Wrong outcomes"),
        ("p50_ms", "lower", "Latency p50"),
    ],
    "sync_correctness": [
        ("errors", "lower", "Errors"),
    ],
}

# Nested key access for sync convergence
SYNC_NESTED = [
    (["convergence", "all_agree"], "equal", True, "Converged"),
    (["convergence", "divergent_cells"], "lower", None, "Divergent cells"),
]


def get_nested(d: dict, keys: list[str]):
    """Get a value from nested dict by key path."""
    for k in keys:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def run_metric(script: str, args: list[str]) -> dict | None:
    """Run a metric script and parse its JSON output."""
    cmd = [sys.executable, str(METRICS_DIR / script), "--json-only", *args]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=600, env=os.environ)
    if result.returncode != 0:
        return None
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return None


def format_delta(baseline_val, current_val, direction: str) -> str:
    """Format a comparison between baseline and current values."""
    if baseline_val is None or current_val is None:
        return "  (no baseline)"

    if isinstance(baseline_val, bool) or isinstance(current_val, bool):
        if baseline_val == current_val:
            return f"  {current_val} (unchanged)"
        return f"  {current_val} (was {baseline_val}) {'!!!' if not current_val else ''}"

    if isinstance(baseline_val, (int, float)) and isinstance(current_val, (int, float)):
        delta = current_val - baseline_val
        pct = (
            (delta / baseline_val) * 100
            if baseline_val != 0
            else (0 if delta == 0 else float("inf"))
        )

        # Determine if this is good, bad, or neutral
        if abs(delta) < 0.01:
            indicator = "="
        elif direction == "lower":
            indicator = "+" if delta > 0 else "-"  # lower is better, so increase is bad
        else:
            indicator = "+" if delta > 0 else "-"  # higher is better, so increase is good

        is_good = (direction == "lower" and delta <= 0) or (direction == "higher" and delta >= 0)
        marker = "" if abs(delta) < 0.01 else (" (better)" if is_good else " (worse)")

        if isinstance(current_val, float):
            val_str = f"{current_val:.1f}"
            delta_str = f"{indicator}{abs(delta):.1f}, {indicator}{abs(pct):.1f}%"
        else:
            val_str = f"{current_val}"
            delta_str = f"{indicator}{abs(delta)}, {indicator}{abs(pct):.1f}%"
        return f"  {val_str}  ({delta_str}){marker}"

    return f"  {current_val} (baseline: {baseline_val})"


def compare_metric(name: str, baseline_data: dict | None, current_data: dict) -> bool:
    """Print comparison for one metric. Returns True if all comparisons are OK."""
    all_ok = True

    if name in COMPARISONS:
        for key, direction, label in COMPARISONS[name]:
            baseline_val = baseline_data.get(key) if baseline_data else None
            current_val = current_data.get(key)
            print(f"  {label + ':':<20}{format_delta(baseline_val, current_val, direction)}")

            # Check for regressions
            if (
                baseline_val is not None
                and current_val is not None
                and isinstance(baseline_val, (int, float))
                and isinstance(current_val, (int, float))
            ):
                delta = current_val - baseline_val
                is_bad = (direction == "lower" and delta > 0) or (
                    direction == "higher" and delta < 0
                )
                # Only flag significant regressions (>20% for latency, any for counts)
                if is_bad:
                    if isinstance(current_val, float) and baseline_val != 0:
                        if abs(delta / baseline_val) > 0.2:
                            all_ok = False
                    elif isinstance(current_val, int) and delta > 0:
                        all_ok = False

    # Special handling for sync nested keys
    if name == "sync_correctness":
        for keys, direction, expected, label in SYNC_NESTED:
            baseline_val = get_nested(baseline_data, keys) if baseline_data else None
            current_val = get_nested(current_data, keys)
            print(f"  {label + ':':<20}{format_delta(baseline_val, current_val, direction)}")
            if direction == "equal" and expected is not None and current_val != expected:
                all_ok = False

    return all_ok


def main():
    parser = argparse.ArgumentParser(
        prog="compare",
        description="Compare metrics against baseline",
    )
    parser.add_argument(
        "--only",
        choices=["latency", "reliability", "sync"],
        default=None,
        help="Run only one metric",
    )
    parser.add_argument(
        "--from",
        dest="from_file",
        default=None,
        help="Compare a saved results JSON instead of running metrics live",
    )
    parser.add_argument(
        "--socket",
        default=None,
        help="Daemon socket path (default: auto-discover)",
    )
    # Tuning for faster runs in worktrees
    parser.add_argument("--latency-executions", type=int, default=20)
    parser.add_argument("--reliability-rounds", type=int, default=18)
    parser.add_argument("--sync-gremlins", type=int, default=2)
    parser.add_argument("--sync-rounds", type=int, default=15)
    args = parser.parse_args()

    # Load baseline
    if BASELINE_PATH.exists():
        baseline = json.loads(BASELINE_PATH.read_text())
        baseline_metrics = baseline.get("metrics", {})
        print(
            f"Baseline: {baseline.get('git_ref', '?')} ({baseline.get('git_branch', '?')}) "
            f"from {baseline.get('generated_at', '?')}",
            file=sys.stderr,
        )
    else:
        baseline_metrics = {}
        print("No baseline found. Results will be shown without comparison.", file=sys.stderr)

    socket_args = ["--socket", args.socket] if args.socket else []

    # Determine which metrics to run
    run_latency = args.only is None or args.only == "latency"
    run_reliability = args.only is None or args.only == "reliability"
    run_sync = args.only is None or args.only == "sync"

    results = {}
    all_ok = True

    if args.from_file:
        # Load saved results instead of running
        saved = json.loads(Path(args.from_file).read_text())
        results = saved.get("metrics", saved)  # handle both wrapper and flat format
    else:
        # Run metrics
        if run_latency:
            print("Running execution-latency...", file=sys.stderr)
            data = run_metric(
                "execution-latency.py",
                ["--executions", str(args.latency_executions), *socket_args],
            )
            if data:
                results["execution_latency"] = data
            else:
                print("  FAILED", file=sys.stderr)

        if run_reliability:
            print("Running kernel-reliability...", file=sys.stderr)
            data = run_metric(
                "kernel-reliability.py",
                ["--rounds", str(args.reliability_rounds), *socket_args],
            )
            if data:
                results["kernel_reliability"] = data
            else:
                print("  FAILED", file=sys.stderr)

        if run_sync:
            print("Running sync-correctness...", file=sys.stderr)
            data = run_metric(
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
            if data:
                results["sync_correctness"] = data
            else:
                print("  FAILED", file=sys.stderr)

    # Print comparison
    print("")
    print("=" * 60)
    print("METRICS COMPARISON")
    print("=" * 60)

    for metric_name in ["execution_latency", "kernel_reliability", "sync_correctness"]:
        if metric_name not in results:
            continue

        friendly = metric_name.replace("_", " ").title()
        print(f"\n{friendly}:")
        baseline_data = baseline_metrics.get(metric_name)
        ok = compare_metric(metric_name, baseline_data, results[metric_name])
        if not ok:
            all_ok = False

    print("")
    if not baseline_metrics:
        print("No baseline to compare against. Run generate-baseline.py on main.")
    elif all_ok:
        print("All metrics within baseline thresholds.")
    else:
        print("REGRESSION DETECTED — some metrics are significantly worse than baseline.")

    # Also dump the raw results JSON for piping
    full_output = {
        "compared_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "baseline_ref": baseline.get("git_ref") if baseline_metrics else None,
        "metrics": results,
        "all_ok": all_ok,
    }
    print("")
    print(json.dumps(full_output, indent=2))

    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
