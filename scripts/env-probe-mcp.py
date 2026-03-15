#!/usr/bin/env python3
"""Tiny MCP server that exposes its own environment for debugging.

Use this to check what env vars an MCP client passes to spawned servers.
Zero dependencies — just needs Python 3.

Example MCP client config:

{
  "command": "python3",
  "args": ["scripts/env-probe-mcp.py"],
  "env": {
    "RUNTIMED_DEV": "1",
    "PROBE_MARKER": "hello"
  }
}

Then ask the agent to call the `get_env` tool.
"""

import atexit
import json
import os
import sys
from pathlib import Path


def read_message():
    """Read a JSON-RPC message from stdin."""
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def write_message(msg):
    """Write a JSON-RPC message to stdout."""
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def handle_initialize(req):
    return {
        "jsonrpc": "2.0",
        "id": req["id"],
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": "env-probe", "version": "0.1.0"},
        },
    }


def handle_tools_list(req):
    return {
        "jsonrpc": "2.0",
        "id": req["id"],
        "result": {
            "tools": [
                {
                    "name": "get_env",
                    "description": "Return all environment variables visible to this MCP server process. Useful for checking what Zed passes through.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "filter": {
                                "type": "string",
                                "description": "Optional substring filter for env var names (case-insensitive)",
                            }
                        },
                    },
                }
            ]
        },
    }


def handle_tools_call(req):
    name = req["params"]["name"]
    args = req["params"].get("arguments", {})

    if name == "get_env":
        env = dict(os.environ)
        filter_str = args.get("filter", "").lower()
        if filter_str:
            env = {k: v for k, v in env.items() if filter_str in k.lower()}

        # Sort for readability
        lines = [f"{k}={v}" for k, v in sorted(env.items())]
        text = "\n".join(lines) if lines else "(no matching env vars)"

        return {
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "content": [{"type": "text", "text": text}],
                "isError": False,
            },
        }

    return {
        "jsonrpc": "2.0",
        "id": req["id"],
        "result": {
            "content": [{"type": "text", "text": f"Unknown tool: {name}"}],
            "isError": True,
        },
    }


def main():
    # Log to stderr so it doesn't interfere with MCP stdio transport
    print("env-probe-mcp: starting", file=sys.stderr)

    # Write pidfile for lifecycle management
    script_dir = Path(__file__).resolve().parent
    project_root = script_dir.parent
    pidfile = project_root / ".context" / "env-probe-mcp.pid"
    pidfile.parent.mkdir(parents=True, exist_ok=True)
    pidfile.write_text(str(os.getpid()))
    atexit.register(lambda: pidfile.unlink(missing_ok=True))
    print(f"env-probe-mcp: pid {os.getpid()} written to {pidfile}", file=sys.stderr)

    while True:
        msg = read_message()
        if msg is None:
            break

        method = msg.get("method", "")

        if method == "initialize":
            write_message(handle_initialize(msg))
        elif method == "notifications/initialized":
            pass  # no response needed
        elif method == "tools/list":
            write_message(handle_tools_list(msg))
        elif method == "tools/call":
            write_message(handle_tools_call(msg))
        elif "id" in msg:
            # Unknown method with an id — respond with method not found
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "error": {"code": -32601, "message": f"Method not found: {method}"},
                }
            )


if __name__ == "__main__":
    main()
