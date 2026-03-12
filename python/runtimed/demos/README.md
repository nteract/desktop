# runtimed demos

Scripts for testing and demonstrating runtimed features. Run from the `python/runtimed/` directory so `uv run` picks up the local dev build.

## Setup

Build the Python bindings (from the repo root):

```bash
cd python/runtimed
uv run --reinstall-package runtimed maturin develop
```

Set the dev daemon socket path:

```bash
# Find your worktree hash
RUNTIMED_DEV=1 ./target/debug/runt daemon status

# Export the socket path (replace <hash> with yours)
export RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock
```

Find open notebook IDs:

```bash
RUNTIMED_DEV=1 ./target/debug/runt notebooks
```

## Demos

### `presence_cursor.py`

Animates a remote cursor across cells in an open notebook. The cursor sweeps through each line of the longest cell, then back across line 0.

```bash
uv run python demos/presence_cursor.py <notebook_id>
```

Requires a notebook window open in the dev app (`cargo xtask dev`). You should see a colored cursor bar with a "peer" label moving through the code.