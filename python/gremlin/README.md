# gremlin

Claude-powered notebook stress tester. Connects to a running notebook via `runtimed` and uses Claude to intelligently (and chaotically) interact with cells.

Internal tool — not published.

## Usage

```bash
# From the repo root, with the dev daemon running:
.venv/bin/python -m gremlin <notebook_id> [prompt]

# Or via the script entrypoint:
.venv/bin/gremlin <notebook_id> [prompt]
```

Requires a Claude Max subscription (no API key needed) via `claude-agent-sdk`.