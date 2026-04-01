"""nteract MCP server launcher.

Finds and exec's the ``runt mcp`` binary shipped with the nteract desktop
app.  The nteract Python package is a thin entry-point — all MCP tools live
in the Rust-native ``runt mcp`` server.

Usage:
    nteract            # via the entry point
    python -m nteract  # module invocation
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import sys
from typing import Any


class _StderrParser(argparse.ArgumentParser):
    """ArgumentParser that always writes to stderr (stdout is MCP's transport)."""

    def _print_message(self, message: str, file: Any = None) -> None:
        super()._print_message(message, file=sys.stderr)


def _find_runt_binary(channel: str) -> str | None:
    """Find the runt binary using the same resolution as mcpb/server/launch.js.

    Search order:
    1. PATH (covers /usr/local/bin/ where the app installer puts the binary)
    2. Platform-specific app bundle / install locations
    """
    binary_name = "runt-nightly" if channel == "nightly" else "runt"
    app_bundle_names = (
        ["nteract Nightly", "nteract-nightly", "nteract (Nightly)"]
        if channel == "nightly"
        else ["nteract"]
    )

    # 1. Check PATH
    found = shutil.which(binary_name)
    if found:
        return found

    # 2. Check platform-specific sidecar / install paths
    home = os.path.expanduser("~")
    system = platform.system()

    candidates: list[str] = []
    if system == "Darwin":
        for name in app_bundle_names:
            candidates.append(f"/Applications/{name}.app/Contents/MacOS/{binary_name}")
            candidates.append(
                os.path.join(home, f"Applications/{name}.app/Contents/MacOS/{binary_name}")
            )
    elif system == "Windows":
        local_app_data = os.environ.get("LOCALAPPDATA", os.path.join(home, "AppData", "Local"))
        for name in app_bundle_names:
            candidates.append(os.path.join(local_app_data, name, f"{binary_name}.exe"))
            candidates.append(os.path.join(local_app_data, "Programs", name, f"{binary_name}.exe"))
    else:  # Linux
        candidates.append(os.path.join(home, ".local", "bin", binary_name))
        for name in app_bundle_names:
            slug = name.lower().replace(" ", "-")
            candidates.append(f"/usr/share/{slug}/{binary_name}")
            candidates.append(f"/opt/{slug}/{binary_name}")

    for path in candidates:
        if os.path.isfile(path):
            return path

    return None


def main():
    """Launch the nteract MCP server.

    Finds and exec's the ``runt mcp`` binary bundled with the nteract
    desktop app.  Exits with a helpful message if the binary is not found.
    """
    parser = _StderrParser(
        prog="nteract",
        description="nteract MCP server — AI-powered Jupyter notebooks.",
    )
    parser.add_argument(
        "--version",
        action="store_true",
        help="Print version and exit.",
    )
    channel_group = parser.add_mutually_exclusive_group()
    channel_group.add_argument(
        "--nightly",
        action="store_true",
        help="Connect to the nteract nightly daemon and open nightly app.",
    )
    channel_group.add_argument(
        "--stable",
        action="store_true",
        help="Connect to the nteract stable daemon and open stable app.",
    )
    parser.add_argument(
        "--no-show",
        action="store_true",
        help="Disable the show_notebook tool (headless environments).",
    )
    args = parser.parse_args()

    if args.version:
        from importlib.metadata import version

        print(f"nteract {version('nteract')}", file=sys.stderr)
        raise SystemExit(0)

    channel = "nightly" if args.nightly else "stable"

    # Find and exec the Rust MCP server (runt mcp)
    binary = _find_runt_binary(channel)
    if binary:
        runt_args = [binary, "mcp"]
        if args.no_show:
            runt_args.append("--no-show")
        print(f"Launching {' '.join(runt_args)}", file=sys.stderr)
        os.execvp(binary, runt_args)
        # execvp never returns

    binary_name = "runt-nightly" if channel == "nightly" else "runt"
    print(
        f"\n[nteract] {binary_name} not found.\n"
        f"\n"
        f"The nteract MCP server requires the nteract desktop app.\n"
        f"\n"
        f"  Download: https://nteract.io\n"
        f"\n"
        f"After installing, restart your MCP client and try again.\n",
        file=sys.stderr,
    )
    raise SystemExit(1)
