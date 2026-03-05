#!/usr/bin/env python3
"""
Presence Demo - Simulate an agent connecting to a notebook room.

This demo connects to an open notebook and periodically updates presence,
moving the cursor between cells. Run this while you have a notebook open
in the nteract desktop app to see the presence indicator appear.

Usage:
    # Connect to a notebook by path (matches what's open in the app)
    python -m runtimed.demos.presence_demo /path/to/notebook.ipynb

    # Connect to a virtual notebook room by ID
    python -m runtimed.demos.presence_demo --notebook-id my-session

    # Customize the agent identity
    python -m runtimed.demos.presence_demo notebook.ipynb \
        --name "Claude Agent" \
        --icon "bot" \
        --color "#7c3aed"

Requirements:
    - The daemon must be running (cargo xtask dev-daemon)
    - For path-based connection, the notebook must be open in the app
"""

import argparse
import random
import sys
import time
from pathlib import Path

# Animal names for random identity generation
ADJECTIVES = ["Swift", "Clever", "Calm", "Bright", "Happy", "Gentle", "Bold", "Wise"]
ANIMALS = ["Cat", "Dog", "Rabbit", "Fish", "Bird", "Squirrel", "Turtle", "Snail"]
ICONS = ["cat", "dog", "rabbit", "fish", "bird", "squirrel", "turtle", "snail"]

# Colors for presence (HSL with good contrast)
COLORS = [
    "hsl(0, 70%, 50%)",    # Red
    "hsl(30, 70%, 50%)",   # Orange
    "hsl(60, 70%, 45%)",   # Yellow
    "hsl(120, 70%, 40%)",  # Green
    "hsl(180, 70%, 40%)",  # Cyan
    "hsl(210, 70%, 50%)",  # Blue
    "hsl(270, 70%, 50%)",  # Purple
    "hsl(330, 70%, 50%)",  # Pink
]


def generate_identity():
    """Generate a random animal-based identity."""
    idx = random.randint(0, len(ANIMALS) - 1)
    adjective = random.choice(ADJECTIVES)
    animal = ANIMALS[idx]
    icon = ICONS[idx]
    color = random.choice(COLORS)
    return f"{adjective} {animal}", icon, color


def main():
    parser = argparse.ArgumentParser(
        description="Presence demo - simulate an agent in a notebook room"
    )
    parser.add_argument(
        "notebook",
        nargs="?",
        help="Path to notebook file or --notebook-id for virtual rooms",
    )
    parser.add_argument(
        "--notebook-id",
        help="Connect to a virtual notebook room by ID (for agent sessions)",
    )
    parser.add_argument(
        "--name",
        help="Display name (default: random animal name)",
    )
    parser.add_argument(
        "--icon",
        help="Lucide icon name (default: matches animal)",
    )
    parser.add_argument(
        "--color",
        help="Cursor color (default: random)",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=3.0,
        help="Seconds between cursor movements (default: 3.0)",
    )
    parser.add_argument(
        "--duration",
        type=float,
        default=60.0,
        help="Total demo duration in seconds (default: 60.0)",
    )

    args = parser.parse_args()

    # Determine notebook ID
    if args.notebook_id:
        notebook_id = args.notebook_id
    elif args.notebook:
        # Use the full path as the notebook ID (matches how the app connects)
        notebook_path = Path(args.notebook).resolve()
        notebook_id = str(notebook_path)
    else:
        print("Error: Provide a notebook path or --notebook-id", file=sys.stderr)
        parser.print_help()
        sys.exit(1)

    # Generate or use provided identity
    if args.name:
        name = args.name
        icon = args.icon or "bot"
        color = args.color or random.choice(COLORS)
    else:
        name, icon, color = generate_identity()
        if args.icon:
            icon = args.icon
        if args.color:
            color = args.color

    print(f"Presence Demo")
    print(f"  Notebook: {notebook_id}")
    print(f"  Identity: {name} ({icon})")
    print(f"  Color: {color}")
    print(f"  Interval: {args.interval}s")
    print(f"  Duration: {args.duration}s")
    print()

    try:
        from runtimed import Session
    except ImportError:
        print("Error: runtimed package not installed.", file=sys.stderr)
        print("Run: cd python/runtimed && uv pip install -e .", file=sys.stderr)
        sys.exit(1)

    # Connect to the notebook room
    print(f"Connecting to notebook room...")
    session = Session(notebook_id=notebook_id)
    session.connect()
    print(f"Connected!")

    # Get cells from the notebook
    cells = session.get_cells()
    if not cells:
        print("No cells in notebook. Creating a sample cell...")
        cell_id = session.create_cell("# Demo cell", "markdown")
        cells = session.get_cells()

    cell_ids = [c.id for c in cells]
    print(f"Found {len(cell_ids)} cells")
    print()

    # Announce presence
    print(f"Announcing presence as '{name}'...")
    session.update_presence(name=name, color=color, icon=icon)

    # Move cursor between cells periodically
    start_time = time.time()
    cursor_idx = 0

    try:
        while time.time() - start_time < args.duration:
            # Move to next cell
            cell_id = cell_ids[cursor_idx % len(cell_ids)]
            print(f"  Moving cursor to cell {cursor_idx % len(cell_ids) + 1}/{len(cell_ids)}: {cell_id[:20]}...")

            session.update_presence(
                name=name,
                color=color,
                icon=icon,
                cell_id=cell_id,
            )

            cursor_idx += 1
            time.sleep(args.interval)

    except KeyboardInterrupt:
        print("\nInterrupted by user")

    # Clear presence by disconnecting
    print("\nDemo complete. Disconnecting...")
    session.close()
    print("Done!")


if __name__ == "__main__":
    main()
