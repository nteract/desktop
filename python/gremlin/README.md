# gremlin

Autonomous notebook agent powered by Claude. Spawns the **nteract MCP server** as a subprocess and lets Claude interact with a live notebook over stdio. Designed to run alongside humans and other gremlins for stress testing and API evaluation.

Internal tool — not published.

## How it works

The gremlin doesn't import `runtimed` or manage notebook sessions directly. Instead, it:

1. Spawns `nteract` (our MCP server) as a stdio subprocess via `claude-agent-sdk`
2. Claude discovers the notebook tools automatically through MCP
3. The agent joins the specified notebook and autonomously decides what to do
4. Everything it does appears in real time in the nteract desktop app

This architecture means the gremlin exercises the **exact same code path** that any MCP client (Zed, Claude Desktop, etc.) would use.

## Usage

```bash
# From the repo root, with the dev daemon running:

# Auto-discover socket path
uv run gremlin <notebook_id>

# Explicit socket path
RUNTIMED_SOCKET_PATH=<path> uv run gremlin <notebook_id>

# Custom prompt
uv run gremlin <notebook_id> "Fix all the errors and add type hints"

# Verbose logging (info / debug)
uv run gremlin -v <notebook_id>
uv run gremlin -vv <notebook_id>

# Limit turns
uv run gremlin --max-turns 10 <notebook_id>

# Or via python -m (equivalent)
uv run python -m gremlin <notebook_id>
```

## Finding notebook IDs

```bash
# List active notebooks via the daemon
uv run python -c "import runtimed; print(runtimed.NativeClient().list_active_notebooks())"
```

## Requirements

- Claude Max subscription (no API key needed) via `claude-agent-sdk`
- A running `runtimed` dev daemon
- The `nteract` package installed in the workspace venv (it is by default)
- Install the agents dependency group: `uv sync --group agents`
