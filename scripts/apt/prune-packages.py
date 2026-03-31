#!/usr/bin/env python3
"""
prune-packages.py — prune a Debian Packages file to keep the N newest entries.

Usage:
    prune-packages.py --packages <path> --keep-last <N> \
                      --output <path> --delete-list <path>

Reads a merged Packages file, splits it into stanzas, sorts by the Version
field (lexicographic — the nightly timestamp suffix YYYYMMDDHHMMSS is
naturally sortable), keeps the newest N, writes the rest's Filename values
to --delete-list for removal from R2.

No third-party dependencies — stdlib only.
"""

import argparse
import sys


def parse_stanzas(text: str) -> list[dict]:
    """Split a Packages file into a list of field dicts, preserving raw text."""
    stanzas = []
    for block in text.split("\n\n"):
        block = block.strip()
        if not block:
            continue
        fields = {}
        current_key = None
        for line in block.splitlines():
            if line.startswith(" ") or line.startswith("\t"):
                # Continuation line — append to current field value
                if current_key:
                    fields[current_key] += "\n" + line
            elif ":" in line:
                key, _, value = line.partition(":")
                current_key = key.strip()
                fields[current_key] = value.strip()
        fields["_raw"] = block
        stanzas.append(fields)
    return stanzas


def main():
    parser = argparse.ArgumentParser(description="Prune a Debian Packages file.")
    parser.add_argument("--packages", required=True, help="Path to merged Packages file")
    parser.add_argument("--keep-last", required=True, type=int, help="Number of versions to keep")
    parser.add_argument("--output", required=True, help="Path to write pruned Packages file")
    parser.add_argument("--delete-list", required=True, help="Path to write R2 keys to delete")
    args = parser.parse_args()

    if args.keep_last < 1:
        print("ERROR: --keep-last must be a positive integer", file=sys.stderr)
        sys.exit(1)

    with open(args.packages) as f:
        text = f.read()

    stanzas = parse_stanzas(text)

    if not stanzas:
        print("WARNING: No stanzas found in Packages file", file=sys.stderr)
        open(args.output, "w").close()
        open(args.delete_list, "w").close()
        sys.exit(0)

    # Sort by Version field — lexicographic order works for nightly timestamps
    stanzas.sort(key=lambda s: s.get("Version", ""))

    to_delete = stanzas[: max(0, len(stanzas) - args.keep_last)]
    to_keep = stanzas[max(0, len(stanzas) - args.keep_last) :]

    print(f"  Total entries:  {len(stanzas)}")
    print(f"  Keeping:        {len(to_keep)}")
    print(f"  Pruning:        {len(to_delete)}")

    with open(args.output, "w") as f:
        f.write("\n\n".join(s["_raw"] for s in to_keep))
        if to_keep:
            f.write("\n")

    with open(args.delete_list, "w") as f:
        for s in to_delete:
            filename = s.get("Filename", "").strip()
            if filename:
                f.write(filename + "\n")
                print(f"  Queued for deletion: {filename}")


if __name__ == "__main__":
    main()
