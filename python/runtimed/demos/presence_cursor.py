"""Demo: animate a cursor across a cell in the desktop app via presence.

Usage (from python/runtimed/, with dev daemon running):

    RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock \
        uv run python demos/presence_cursor.py [notebook_id]

If no notebook_id is provided, auto-detects the first open notebook.
"""

import os
import sys
import time

import runtimed


def get_socket_path():
    path = os.environ.get("RUNTIMED_SOCKET_PATH")
    if not path:
        print(
            "Set RUNTIMED_SOCKET_PATH to your dev daemon socket, e.g.:\n"
            "  RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/worktrees/<hash>/runtimed.sock\n"
            "\n"
            "Find the hash with: RUNTIMED_DEV=1 ./target/debug/runt daemon status",
            file=sys.stderr,
        )
        sys.exit(1)
    return path


def main():
    get_socket_path()  # validate env var is set

    notebook_id = sys.argv[1] if len(sys.argv) > 1 else None

    if not notebook_id:
        # Auto-detect: pick the first open notebook
        client = runtimed.DaemonClient()
        rooms = client.list_rooms()
        if not rooms:
            print("No open notebooks found. Open a notebook in nteract first.", file=sys.stderr)
            sys.exit(1)
        notebook_id = rooms[0]["notebook_id"]
        print(f"Auto-detected notebook: {notebook_id}")

    session = runtimed.Session(notebook_id=notebook_id, peer_label="🦾")
    session.connect()

    cells = session.get_cells()
    if not cells:
        print("No cells found in notebook", file=sys.stderr)
        sys.exit(1)

    print("Cells:")
    for c in cells:
        print(f"  {c.id}: {c.cell_type} ({len(c.source)} chars) -> {c.source[:60]!r}")

    # Pick the cell with the most content
    cell = max(cells, key=lambda c: len(c.source))
    source = cell.source
    lines = source.split("\n")
    print(f"\nAnimating cursor across cell {cell.id} ({len(source)} chars, {len(lines)} lines)")

    # Sweep across each line
    for line_num, line_text in enumerate(lines):
        for col in range(len(line_text) + 1):
            session.set_cursor(cell.id, line=line_num, column=col)
            time.sleep(0.04)

    # Sweep back across line 0
    first_line = lines[0] if lines else ""
    for col in range(len(first_line), -1, -1):
        session.set_cursor(cell.id, line=0, column=col)
        time.sleep(0.04)

    # Hold at the beginning so you can see it
    session.set_cursor(cell.id, line=0, column=0)
    time.sleep(2)

    print("Done.")


if __name__ == "__main__":
    main()
