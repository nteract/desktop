"""Demo: animate cursor and selection across cells via presence.

Showcases:
- Cell focus indicators (colored dots appear when cursor enters a cell)
- Cursor animation (cursor bar moves through code)
- Selection highlighting (selected text is highlighted)
- Multi-cell navigation (cursor jumps between cells)

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


def demo_cell_focus(session, cells):
    """Demo 1: Cell focus indicators - jump between cells to show colored dots."""
    print("\n📍 Demo 1: Cell Focus Indicators")
    print("   Watch the colored dots appear in the cell gutter as we switch cells...")

    for i, cell in enumerate(cells[:4]):  # Show first 4 cells
        print(f"   → Focusing cell {i + 1}: {cell.cell_type}")
        session.set_cursor(cell.id, line=0, column=0)
        time.sleep(1.0)


def demo_cursor_animation(session, cell):
    """Demo 2: Animate cursor across a cell."""
    source = cell.source
    lines = source.split("\n")
    print("\n✏️  Demo 2: Cursor Animation")
    print(f"   Sweeping cursor across {len(lines)} lines...")

    # Sweep across each line
    for line_num, line_text in enumerate(lines[:5]):  # Limit to 5 lines for demo
        for col in range(len(line_text) + 1):
            session.set_cursor(cell.id, line=line_num, column=col)
            time.sleep(0.03)

    # Sweep back across line 0
    first_line = lines[0] if lines else ""
    for col in range(len(first_line), -1, -1):
        session.set_cursor(cell.id, line=0, column=col)
        time.sleep(0.03)


def demo_selection(session, cell):
    """Demo 3: Selection highlighting - grow selection across the cell."""
    source = cell.source
    lines = source.split("\n")
    print("\n🎨 Demo 3: Selection Highlighting")
    print("   Growing selection across the code...")

    # Start at beginning
    anchor_line, anchor_col = 0, 0

    # Grow selection line by line
    for line_num, line_text in enumerate(lines[:5]):  # Limit to 5 lines
        for col in range(0, len(line_text) + 1, 3):  # Skip every 3 cols for speed
            session.set_selection(
                cell.id,
                anchor_line=anchor_line,
                anchor_col=anchor_col,
                head_line=line_num,
                head_col=col,
            )
            time.sleep(0.05)

    # Hold the full selection
    time.sleep(1.0)

    # Shrink selection back
    print("   Shrinking selection...")
    for line_num in range(min(4, len(lines) - 1), -1, -1):
        session.set_selection(
            cell.id,
            anchor_line=anchor_line,
            anchor_col=anchor_col,
            head_line=line_num,
            head_col=0,
        )
        time.sleep(0.15)

    # Clear selection by setting cursor
    session.set_cursor(cell.id, line=0, column=0)


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

    session = runtimed.Session(notebook_id=notebook_id, peer_label="🤖 Agent")
    session.connect()

    cells = session.get_cells()
    if not cells:
        print("No cells found in notebook", file=sys.stderr)
        sys.exit(1)

    print("=" * 50)
    print("🎭 Presence Demo: Cell Focus & Selection")
    print("=" * 50)
    print(f"\nNotebook: {notebook_id}")
    print(f"Found {len(cells)} cells:")
    for i, c in enumerate(cells[:5]):
        preview = c.source[:40].replace("\n", "↵") if c.source else "(empty)"
        print(f"  [{i + 1}] {c.cell_type}: {preview}...")

    # Run demos
    demo_cell_focus(session, cells)

    # Pick cell with most content for cursor/selection demos
    cell = max(cells, key=lambda c: len(c.source))
    if len(cell.source) > 10:
        demo_cursor_animation(session, cell)
        demo_selection(session, cell)

    # Final: return to first cell
    print("\n✅ Demo complete!")
    print("   Cursor returning to first cell...")
    session.set_cursor(cells[0].id, line=0, column=0)
    time.sleep(2)


if __name__ == "__main__":
    main()
