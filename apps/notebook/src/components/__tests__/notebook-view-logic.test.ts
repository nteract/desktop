/**
 * Tests for NotebookView's non-trivial logic:
 * - isCellFullyHidden: metadata-driven cell visibility
 * - computeHiddenGroups: consecutive hidden cell grouping with error counts
 * - calculateDragAfterCellId: drag-and-drop position calculation
 * - stableDomOrder: DOM ordering invariant that prevents iframe reloads
 */

import { describe, expect, it } from "vite-plus/test";
import type { CodeCell, NotebookCell } from "../../types";

// ── Extracted logic from NotebookView.tsx ───────────────────────────

/**
 * Mirrors isCellFullyHidden from NotebookView.tsx.
 * A cell is fully hidden when:
 * - It's a code cell (markdown/raw can't be hidden)
 * - source_hidden is true
 * - AND either outputs_hidden is true OR there are no outputs
 *
 * Intentionally does NOT check outputs.length for the outputs_hidden case,
 * so cells stay collapsed when outputs are transiently cleared during re-execution.
 */
function isCellFullyHidden(cell: NotebookCell): boolean {
  if (cell.cell_type !== "code") return false;
  const jupyter = cell.metadata?.jupyter as
    | { source_hidden?: boolean; outputs_hidden?: boolean }
    | undefined;
  if (!jupyter?.source_hidden) return false;
  return jupyter.outputs_hidden === true || cell.outputs.length === 0;
}

/**
 * Mirrors the hiddenGroups useMemo from NotebookView.tsx.
 * Groups consecutive fully-hidden cells and tracks error counts per group.
 */
function computeHiddenGroups(cells: NotebookCell[]) {
  const groups = new Map<
    string,
    {
      count: number;
      isFirst: boolean;
      groupCellIds: string[];
      errorCount: number;
    }
  >();
  let i = 0;
  while (i < cells.length) {
    if (isCellFullyHidden(cells[i])) {
      const groupCellIds: string[] = [];
      let groupErrorCount = 0;
      while (i < cells.length && isCellFullyHidden(cells[i])) {
        const c = cells[i];
        groupCellIds.push(c.id);
        if (c.cell_type === "code") {
          groupErrorCount += c.outputs.filter((o) => o.output_type === "error").length;
        }
        i++;
      }
      for (let j = 0; j < groupCellIds.length; j++) {
        groups.set(groupCellIds[j], {
          count: groupCellIds.length,
          isFirst: j === 0,
          groupCellIds,
          errorCount: groupErrorCount,
        });
      }
    } else {
      i++;
    }
  }
  return groups;
}

/**
 * Mirrors the drag-end afterCellId calculation from NotebookView.tsx.
 * Determines where to place a cell after a drag operation.
 */
function calculateDragAfterCellId(
  cellIds: string[],
  oldIndex: number,
  newIndex: number,
): string | null {
  if (newIndex === 0) {
    return null;
  }
  if (newIndex > oldIndex) {
    // Moving down: place after the cell at newIndex
    return cellIds[newIndex];
  }
  // Moving up: place after the cell at newIndex - 1
  return newIndex > 0 ? cellIds[newIndex - 1] : null;
}

// ── Helpers ────────────────────────────────────────────────────────

function makeCodeCell(
  id: string,
  overrides: {
    source_hidden?: boolean;
    outputs_hidden?: boolean;
    outputs?: CodeCell["outputs"];
  } = {},
): CodeCell {
  const jupyter: Record<string, boolean> = {};
  if (overrides.source_hidden !== undefined) jupyter.source_hidden = overrides.source_hidden;
  if (overrides.outputs_hidden !== undefined) jupyter.outputs_hidden = overrides.outputs_hidden;

  return {
    cell_type: "code",
    id,
    source: `print("${id}")`,
    execution_count: null,
    outputs: overrides.outputs ?? [],
    metadata: Object.keys(jupyter).length > 0 ? { jupyter } : {},
  };
}

function makeMarkdownCell(id: string): NotebookCell {
  return {
    cell_type: "markdown",
    id,
    source: `# ${id}`,
    metadata: {},
  };
}

function makeErrorOutput(): CodeCell["outputs"][number] {
  return {
    output_type: "error",
    ename: "ValueError",
    evalue: "bad value",
    traceback: ["Traceback ..."],
  };
}

function makeStreamOutput(): CodeCell["outputs"][number] {
  return { output_type: "stream", name: "stdout", text: "hello" };
}

// ── Tests ──────────────────────────────────────────────────────────

describe("isCellFullyHidden", () => {
  it("returns false for markdown cells regardless of metadata", () => {
    const cell = makeMarkdownCell("md-1");
    // Even if someone manually set jupyter metadata on a markdown cell
    cell.metadata = { jupyter: { source_hidden: true, outputs_hidden: true } };
    expect(isCellFullyHidden(cell)).toBe(false);
  });

  it("returns false for code cells without source_hidden", () => {
    expect(isCellFullyHidden(makeCodeCell("c1"))).toBe(false);
  });

  it("returns false when only source_hidden is true and outputs exist but are not hidden", () => {
    const cell = makeCodeCell("c1", {
      source_hidden: true,
      outputs: [makeStreamOutput()],
    });
    expect(isCellFullyHidden(cell)).toBe(false);
  });

  it("returns true when source_hidden and outputs_hidden are both true", () => {
    const cell = makeCodeCell("c1", {
      source_hidden: true,
      outputs_hidden: true,
      outputs: [makeStreamOutput()],
    });
    expect(isCellFullyHidden(cell)).toBe(true);
  });

  it("returns true when source_hidden is true and there are no outputs (even without outputs_hidden)", () => {
    const cell = makeCodeCell("c1", {
      source_hidden: true,
      outputs: [],
    });
    expect(isCellFullyHidden(cell)).toBe(true);
  });

  it("stays hidden when outputs are cleared during re-execution (outputs_hidden=true, empty outputs)", () => {
    // This tests the intentional behavior: outputs_hidden=true means stay hidden
    // even when outputs are transiently empty during re-execution
    const cell = makeCodeCell("c1", {
      source_hidden: true,
      outputs_hidden: true,
      outputs: [],
    });
    expect(isCellFullyHidden(cell)).toBe(true);
  });

  it("returns false for raw cells", () => {
    const cell: NotebookCell = {
      cell_type: "raw",
      id: "r1",
      source: "raw content",
      metadata: { jupyter: { source_hidden: true } },
    };
    expect(isCellFullyHidden(cell)).toBe(false);
  });
});

describe("computeHiddenGroups", () => {
  it("returns empty map when no cells are hidden", () => {
    const cells = [makeCodeCell("a"), makeCodeCell("b"), makeCodeCell("c")];
    expect(computeHiddenGroups(cells).size).toBe(0);
  });

  it("groups a single hidden cell", () => {
    const cells = [
      makeCodeCell("a"),
      makeCodeCell("b", { source_hidden: true }),
      makeCodeCell("c"),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.size).toBe(1);
    const group = groups.get("b")!;
    expect(group.count).toBe(1);
    expect(group.isFirst).toBe(true);
    expect(group.groupCellIds).toEqual(["b"]);
  });

  it("groups consecutive hidden cells together", () => {
    const cells = [
      makeCodeCell("a"),
      makeCodeCell("b", { source_hidden: true }),
      makeCodeCell("c", { source_hidden: true }),
      makeCodeCell("d", { source_hidden: true }),
      makeCodeCell("e"),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.size).toBe(3);

    // First cell in group is marked isFirst
    expect(groups.get("b")!.isFirst).toBe(true);
    expect(groups.get("c")!.isFirst).toBe(false);
    expect(groups.get("d")!.isFirst).toBe(false);

    // All share the same count and groupCellIds
    expect(groups.get("b")!.count).toBe(3);
    expect(groups.get("c")!.count).toBe(3);
    expect(groups.get("d")!.count).toBe(3);
    expect(groups.get("b")!.groupCellIds).toEqual(["b", "c", "d"]);
  });

  it("creates separate groups for non-consecutive hidden cells", () => {
    const cells = [
      makeCodeCell("a", { source_hidden: true }),
      makeCodeCell("b"), // visible — breaks the group
      makeCodeCell("c", { source_hidden: true }),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.size).toBe(2);
    expect(groups.get("a")!.groupCellIds).toEqual(["a"]);
    expect(groups.get("c")!.groupCellIds).toEqual(["c"]);
  });

  it("does not group markdown cells even with hidden metadata", () => {
    const md = makeMarkdownCell("md");
    md.metadata = { jupyter: { source_hidden: true, outputs_hidden: true } };
    const cells = [
      makeCodeCell("a", { source_hidden: true }),
      md, // breaks the group — markdown can't be hidden
      makeCodeCell("c", { source_hidden: true }),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.size).toBe(2);
    expect(groups.has("md")).toBe(false);
  });

  it("counts errors across all cells in a group", () => {
    const cells = [
      makeCodeCell("a", {
        source_hidden: true,
        outputs_hidden: true,
        outputs: [makeErrorOutput(), makeErrorOutput()],
      }),
      makeCodeCell("b", {
        source_hidden: true,
        outputs_hidden: true,
        outputs: [makeStreamOutput(), makeErrorOutput()],
      }),
    ];
    const groups = computeHiddenGroups(cells);
    // 2 errors in cell a + 1 error in cell b = 3 total
    expect(groups.get("a")!.errorCount).toBe(3);
    expect(groups.get("b")!.errorCount).toBe(3);
  });

  it("reports zero errors when group has no error outputs", () => {
    const cells = [
      makeCodeCell("a", {
        source_hidden: true,
        outputs_hidden: true,
        outputs: [makeStreamOutput()],
      }),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.get("a")!.errorCount).toBe(0);
  });

  it("handles all cells hidden", () => {
    const cells = [
      makeCodeCell("a", { source_hidden: true }),
      makeCodeCell("b", { source_hidden: true }),
    ];
    const groups = computeHiddenGroups(cells);
    expect(groups.size).toBe(2);
    expect(groups.get("a")!.isFirst).toBe(true);
    expect(groups.get("b")!.isFirst).toBe(false);
    expect(groups.get("a")!.count).toBe(2);
  });

  it("handles empty cell list", () => {
    expect(computeHiddenGroups([]).size).toBe(0);
  });
});

describe("calculateDragAfterCellId", () => {
  const cellIds = ["a", "b", "c", "d", "e"];

  it("returns null when moving to position 0 (beginning)", () => {
    expect(calculateDragAfterCellId(cellIds, 3, 0)).toBeNull();
  });

  it("places after the cell at newIndex when moving down", () => {
    // Moving cell at index 1 to index 3: place after cellIds[3] = "d"
    expect(calculateDragAfterCellId(cellIds, 1, 3)).toBe("d");
  });

  it("places after cell above target when moving up", () => {
    // Moving cell at index 4 to index 2: place after cellIds[1] = "b"
    expect(calculateDragAfterCellId(cellIds, 4, 2)).toBe("b");
  });

  it("returns null when moving up to index 0", () => {
    // Moving cell at index 3 to index 0: beginning
    expect(calculateDragAfterCellId(cellIds, 3, 0)).toBeNull();
  });

  it("handles moving down by one position", () => {
    // Moving cell at index 1 to index 2: place after cellIds[2] = "c"
    expect(calculateDragAfterCellId(cellIds, 1, 2)).toBe("c");
  });

  it("handles moving up by one position", () => {
    // Moving cell at index 2 to index 1: place after cellIds[0] = "a"
    expect(calculateDragAfterCellId(cellIds, 2, 1)).toBe("a");
  });

  it("handles moving to last position", () => {
    // Moving cell at index 0 to index 4: place after cellIds[4] = "e"
    expect(calculateDragAfterCellId(cellIds, 0, 4)).toBe("e");
  });
});

describe("stableDomOrder invariant", () => {
  it("sorts cell IDs alphabetically for stable DOM rendering", () => {
    const cellIds = ["c-uuid", "a-uuid", "b-uuid"];
    const stableDomOrder = [...cellIds].sort();
    expect(stableDomOrder).toEqual(["a-uuid", "b-uuid", "c-uuid"]);
  });

  it("preserves order identity when cellIds are reordered (cell move)", () => {
    // Before move: a, b, c
    const before = ["a", "b", "c"];
    const stableBefore = [...before].sort();

    // After move (c moved to front): c, a, b
    const after = ["c", "a", "b"];
    const stableAfter = [...after].sort();

    // DOM order should be identical — React won't move any nodes
    expect(stableBefore).toEqual(stableAfter);
  });

  it("produces consistent DOM order regardless of insertion order", () => {
    // Adding a cell with id "d" at different positions gives same DOM order
    const withDAtStart = ["d", "a", "b", "c"];
    const withDAtEnd = ["a", "b", "c", "d"];
    const withDInMiddle = ["a", "d", "b", "c"];

    expect([...withDAtStart].sort()).toEqual([...withDAtEnd].sort());
    expect([...withDAtEnd].sort()).toEqual([...withDInMiddle].sort());
  });

  it("only changes DOM order when cells are added or removed", () => {
    const original = ["b", "a", "c"];
    const stableOriginal = [...original].sort();

    // Adding "d" inserts one new node
    const withNew = ["b", "a", "c", "d"];
    const stableWithNew = [...withNew].sort();
    // All original IDs still in same relative order
    const originalOnly = stableWithNew.filter((id) => original.includes(id));
    expect(originalOnly).toEqual(stableOriginal);

    // Removing "a" removes one node, others stay put
    const withRemoved = ["b", "c"];
    const stableWithRemoved = [...withRemoved].sort();
    expect(stableWithRemoved).toEqual(["b", "c"]);
  });
});

describe("navigation with hidden group skipping", () => {
  /**
   * Mirrors the isVisibleCell + navigation loop from NotebookView's renderCell.
   * When navigating, we skip cells that are in a hidden group but not the
   * first cell of that group.
   */
  function findNextVisibleIndex(
    cellIds: string[],
    currentIndex: number,
    hiddenGroups: Map<string, { isFirst: boolean }>,
    direction: "forward" | "backward",
  ): number {
    const delta = direction === "forward" ? 1 : -1;
    let idx = currentIndex + delta;
    while (idx >= 0 && idx < cellIds.length) {
      const g = hiddenGroups.get(cellIds[idx]);
      if (!g || g.isFirst) return idx;
      idx += delta;
    }
    return -1; // no visible cell found
  }

  it("skips hidden cells when navigating forward", () => {
    const cellIds = ["a", "b", "c", "d", "e"];
    // b, c, d are in a hidden group; only b is isFirst
    const hiddenGroups = new Map([
      ["b", { isFirst: true }],
      ["c", { isFirst: false }],
      ["d", { isFirst: false }],
    ]);

    // From "a" (index 0), next visible is "b" (isFirst=true)
    expect(findNextVisibleIndex(cellIds, 0, hiddenGroups, "forward")).toBe(1);

    // From "b" (index 1), skip c and d (not isFirst), land on "e"
    expect(findNextVisibleIndex(cellIds, 1, hiddenGroups, "forward")).toBe(4);
  });

  it("skips hidden cells when navigating backward", () => {
    const cellIds = ["a", "b", "c", "d", "e"];
    const hiddenGroups = new Map([
      ["b", { isFirst: true }],
      ["c", { isFirst: false }],
      ["d", { isFirst: false }],
    ]);

    // From "e" (index 4), back skips d and c (not isFirst), lands on "b" (isFirst)
    expect(findNextVisibleIndex(cellIds, 4, hiddenGroups, "backward")).toBe(1);

    // From "b" (index 1), back to "a"
    expect(findNextVisibleIndex(cellIds, 1, hiddenGroups, "backward")).toBe(0);
  });

  it("returns -1 when no visible cell exists in direction", () => {
    const cellIds = ["a", "b", "c"];
    const hiddenGroups = new Map([
      ["b", { isFirst: false }],
      ["c", { isFirst: false }],
    ]);

    // From "a" (index 0), forward: b and c are non-first hidden
    expect(findNextVisibleIndex(cellIds, 0, hiddenGroups, "forward")).toBe(-1);
  });

  it("works with no hidden groups (normal navigation)", () => {
    const cellIds = ["a", "b", "c"];
    const hiddenGroups = new Map<string, { isFirst: boolean }>();

    expect(findNextVisibleIndex(cellIds, 0, hiddenGroups, "forward")).toBe(1);
    expect(findNextVisibleIndex(cellIds, 2, hiddenGroups, "backward")).toBe(1);
  });

  it("handles adjacent hidden groups correctly", () => {
    const cellIds = ["a", "b", "c", "d", "e"];
    // Two separate groups: [b] and [d]
    // b is isFirst of its group, d is isFirst of its group
    const hiddenGroups = new Map([
      ["b", { isFirst: true }],
      ["d", { isFirst: true }],
    ]);

    // All cells are visible (isFirst counts as visible)
    expect(findNextVisibleIndex(cellIds, 0, hiddenGroups, "forward")).toBe(1);
    expect(findNextVisibleIndex(cellIds, 1, hiddenGroups, "forward")).toBe(2);
    expect(findNextVisibleIndex(cellIds, 2, hiddenGroups, "forward")).toBe(3);
  });
});
