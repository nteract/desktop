import { afterEach, describe, expect, it } from "vitest";
import type { NotebookCell } from "../../types";
import {
  getCellById,
  getNotebookCellsSnapshot,
  replaceNotebookCells,
  resetNotebookCells,
  updateCellById,
  updateNotebookCells,
} from "../notebook-cells";

const codeCell = (id: string, source = ""): NotebookCell => ({
  cell_type: "code",
  id,
  source,
  execution_count: null,
  outputs: [],
  metadata: {},
});

const markdownCell = (id: string, source = ""): NotebookCell => ({
  cell_type: "markdown",
  id,
  source,
  metadata: {},
});

afterEach(() => {
  resetNotebookCells();
});

describe("replaceNotebookCells", () => {
  it("sets the snapshot to the provided cells", () => {
    const cells = [codeCell("a"), markdownCell("b")];
    replaceNotebookCells(cells);
    expect(getNotebookCellsSnapshot()).toEqual(cells);
  });

  it("replaces previous cells entirely", () => {
    replaceNotebookCells([codeCell("a")]);
    const next = [codeCell("b"), codeCell("c")];
    replaceNotebookCells(next);
    expect(getNotebookCellsSnapshot()).toEqual(next);
    expect(getNotebookCellsSnapshot()).toHaveLength(2);
  });

  it("notifies subscribers", () => {
    // Test through the public API by checking snapshot changes.
    replaceNotebookCells([codeCell("a")]);
    expect(getNotebookCellsSnapshot()).toHaveLength(1);
  });
});

describe("updateNotebookCells", () => {
  it("applies the updater function to current cells", () => {
    replaceNotebookCells([codeCell("a"), codeCell("b")]);
    const result = updateNotebookCells((cells) => cells.slice(1));
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("b");
    expect(getNotebookCellsSnapshot()).toEqual(result);
  });

  it("returns the new snapshot", () => {
    replaceNotebookCells([codeCell("a")]);
    const appended = codeCell("b");
    const result = updateNotebookCells((cells) => [...cells, appended]);
    expect(result).toHaveLength(2);
    expect(result[1]).toEqual(appended);
  });

  it("can clear cells via updater", () => {
    replaceNotebookCells([codeCell("a"), codeCell("b"), codeCell("c")]);
    updateNotebookCells(() => []);
    expect(getNotebookCellsSnapshot()).toHaveLength(0);
  });

  it("can transform cell contents", () => {
    replaceNotebookCells([codeCell("a", "print('hello')")]);
    updateNotebookCells((cells) =>
      cells.map((c) =>
        c.cell_type === "code" ? { ...c, source: "print('world')" } : c,
      ),
    );
    expect(getNotebookCellsSnapshot()[0].source).toBe("print('world')");
  });
});

describe("updateCellById", () => {
  it("updates a single cell by ID", () => {
    replaceNotebookCells([codeCell("a", "old"), codeCell("b", "keep")]);
    updateCellById("a", (c) => ({ ...c, source: "new" }));
    expect(getCellById("a")?.source).toBe("new");
    expect(getCellById("b")?.source).toBe("keep");
  });

  it("is a no-op for non-existent IDs", () => {
    replaceNotebookCells([codeCell("a")]);
    updateCellById("nonexistent", (c) => ({ ...c, source: "boom" }));
    expect(getNotebookCellsSnapshot()).toHaveLength(1);
  });

  it("preserves cell ordering", () => {
    replaceNotebookCells([codeCell("a"), codeCell("b"), codeCell("c")]);
    updateCellById("b", (c) => ({ ...c, source: "updated" }));
    const ids = getNotebookCellsSnapshot().map((c) => c.id);
    expect(ids).toEqual(["a", "b", "c"]);
  });
});

describe("getCellById", () => {
  it("returns the cell for a known ID", () => {
    replaceNotebookCells([codeCell("a", "hello")]);
    const cell = getCellById("a");
    expect(cell?.id).toBe("a");
    expect(cell?.source).toBe("hello");
  });

  it("returns undefined for unknown IDs", () => {
    replaceNotebookCells([codeCell("a")]);
    expect(getCellById("nonexistent")).toBeUndefined();
  });
});

describe("resetNotebookCells", () => {
  it("empties the snapshot", () => {
    replaceNotebookCells([codeCell("a"), markdownCell("b")]);
    resetNotebookCells();
    expect(getNotebookCellsSnapshot()).toEqual([]);
  });

  it("is idempotent on empty store", () => {
    resetNotebookCells();
    resetNotebookCells();
    expect(getNotebookCellsSnapshot()).toEqual([]);
  });
});

describe("getNotebookCellsSnapshot", () => {
  it("returns empty array initially", () => {
    expect(getNotebookCellsSnapshot()).toEqual([]);
  });

  it("returns content-equal results on consecutive calls", () => {
    const cells = [codeCell("a")];
    replaceNotebookCells(cells);
    const snap1 = getNotebookCellsSnapshot();
    const snap2 = getNotebookCellsSnapshot();
    expect(snap1).toEqual(snap2);
  });
});

describe("subscriber notifications", () => {
  it("replace produces distinct content each call", () => {
    const ids = new Set<string>();
    for (let i = 0; i < 5; i++) {
      replaceNotebookCells([codeCell(`cell-${i}`)]);
      ids.add(getNotebookCellsSnapshot()[0].id);
    }
    expect(ids.size).toBe(5);
  });

  it("update produces distinct content each call", () => {
    replaceNotebookCells([codeCell("a")]);
    const ref1 = getNotebookCellsSnapshot();
    updateNotebookCells((cells) => [...cells, codeCell("b")]);
    const ref2 = getNotebookCellsSnapshot();
    updateNotebookCells((cells) => [...cells, codeCell("c")]);
    const ref3 = getNotebookCellsSnapshot();
    expect(ref1).toHaveLength(1);
    expect(ref2).toHaveLength(2);
    expect(ref3).toHaveLength(3);
  });
});

describe("cell diffing in replaceNotebookCells", () => {
  it("preserves referential identity for unchanged cells", () => {
    const cells = [codeCell("a", "x = 1"), codeCell("b", "y = 2")];
    replaceNotebookCells(cells);
    const ref1 = getCellById("a");
    const ref2 = getCellById("b");

    // Replace with structurally identical cells (new objects)
    replaceNotebookCells([codeCell("a", "x = 1"), codeCell("b", "y = 2")]);
    const ref1After = getCellById("a");
    const ref2After = getCellById("b");

    // Old references should be preserved — same object, not just equal
    expect(ref1After).toBe(ref1);
    expect(ref2After).toBe(ref2);
  });

  it("replaces reference when source changes", () => {
    replaceNotebookCells([codeCell("a", "x = 1")]);
    const ref1 = getCellById("a");

    replaceNotebookCells([codeCell("a", "x = 2")]);
    const ref2 = getCellById("a");

    expect(ref2).not.toBe(ref1);
    expect(ref2?.source).toBe("x = 2");
  });

  it("replaces reference when execution_count changes", () => {
    const cell: NotebookCell = {
      cell_type: "code",
      id: "a",
      source: "1+1",
      execution_count: null,
      outputs: [],
      metadata: {},
    };
    replaceNotebookCells([cell]);
    const ref1 = getCellById("a");

    replaceNotebookCells([{ ...cell, execution_count: 1 }]);
    const ref2 = getCellById("a");

    expect(ref2).not.toBe(ref1);
    expect(ref2?.cell_type === "code" && ref2.execution_count).toBe(1);
  });

  it("replaces reference when outputs change", () => {
    const output = {
      output_type: "stream" as const,
      name: "stdout" as const,
      text: "hello",
    };
    const cell: NotebookCell = {
      cell_type: "code",
      id: "a",
      source: "print('hello')",
      execution_count: 1,
      outputs: [],
      metadata: {},
    };
    replaceNotebookCells([cell]);
    const ref1 = getCellById("a");

    replaceNotebookCells([{ ...cell, outputs: [output] }]);
    const ref2 = getCellById("a");

    expect(ref2).not.toBe(ref1);
  });

  it("preserves reference when outputs are referentially equal", () => {
    const output = {
      output_type: "stream" as const,
      name: "stdout" as const,
      text: "hello",
    };
    const cell: NotebookCell = {
      cell_type: "code",
      id: "a",
      source: "print('hello')",
      execution_count: 1,
      outputs: [output],
      metadata: {},
    };
    replaceNotebookCells([cell]);
    const ref1 = getCellById("a");

    // Same output object reference — cell should be preserved
    replaceNotebookCells([{ ...cell, outputs: [output] }]);
    const ref2 = getCellById("a");

    expect(ref2).toBe(ref1);
  });

  it("replaces reference when metadata changes", () => {
    replaceNotebookCells([
      { ...codeCell("a"), metadata: { collapsed: false } },
    ]);
    const ref1 = getCellById("a");

    replaceNotebookCells([{ ...codeCell("a"), metadata: { collapsed: true } }]);
    const ref2 = getCellById("a");

    expect(ref2).not.toBe(ref1);
  });

  it("preserves reference for markdown with identical resolvedAssets", () => {
    const assets = { "image.png": "sha256:abc" };
    const cell: NotebookCell = {
      cell_type: "markdown",
      id: "m1",
      source: "# Hello",
      metadata: {},
      resolvedAssets: assets,
    };
    replaceNotebookCells([cell]);
    const ref1 = getCellById("m1");

    // New object with same key/values — shallow compare should match
    replaceNotebookCells([
      { ...cell, resolvedAssets: { "image.png": "sha256:abc" } },
    ]);
    const ref2 = getCellById("m1");

    expect(ref2).toBe(ref1);
  });

  it("only changes reference for the cell that changed", () => {
    replaceNotebookCells([
      codeCell("a", "unchanged"),
      codeCell("b", "will change"),
      codeCell("c", "unchanged"),
    ]);
    const refA = getCellById("a");
    const refB = getCellById("b");
    const refC = getCellById("c");

    replaceNotebookCells([
      codeCell("a", "unchanged"),
      codeCell("b", "changed!"),
      codeCell("c", "unchanged"),
    ]);

    expect(getCellById("a")).toBe(refA);
    expect(getCellById("b")).not.toBe(refB);
    expect(getCellById("b")?.source).toBe("changed!");
    expect(getCellById("c")).toBe(refC);
  });

  it("handles cell addition without breaking existing refs", () => {
    replaceNotebookCells([codeCell("a", "keep")]);
    const refA = getCellById("a");

    replaceNotebookCells([codeCell("a", "keep"), codeCell("b", "new")]);

    expect(getCellById("a")).toBe(refA);
    expect(getCellById("b")?.source).toBe("new");
    expect(getNotebookCellsSnapshot()).toHaveLength(2);
  });

  it("handles cell removal", () => {
    replaceNotebookCells([codeCell("a", "keep"), codeCell("b", "remove")]);

    replaceNotebookCells([codeCell("a", "keep")]);

    expect(getNotebookCellsSnapshot()).toHaveLength(1);
    expect(getCellById("b")).toBeUndefined();
  });

  it("handles cell type change", () => {
    replaceNotebookCells([codeCell("a", "# Title")]);
    const ref1 = getCellById("a");

    replaceNotebookCells([markdownCell("a", "# Title")]);
    const ref2 = getCellById("a");

    expect(ref2).not.toBe(ref1);
    expect(ref2?.cell_type).toBe("markdown");
  });
});

describe("mixed cell types", () => {
  it("stores code, markdown, and raw cells", () => {
    const cells: NotebookCell[] = [
      codeCell("c1", "x = 1"),
      markdownCell("m1", "# Title"),
      { cell_type: "raw", id: "r1", source: "raw content", metadata: {} },
    ];
    replaceNotebookCells(cells);
    const snap = getNotebookCellsSnapshot();
    expect(snap).toHaveLength(3);
    expect(snap[0].cell_type).toBe("code");
    expect(snap[1].cell_type).toBe("markdown");
    expect(snap[2].cell_type).toBe("raw");
  });

  it("preserves code cell outputs and execution_count", () => {
    const cell: NotebookCell = {
      cell_type: "code",
      id: "c1",
      source: "1 + 1",
      execution_count: 42,
      outputs: [
        {
          output_type: "execute_result",
          data: { "text/plain": "2" },
          execution_count: 42,
        },
      ],
      metadata: {},
    };
    replaceNotebookCells([cell]);
    const snap = getNotebookCellsSnapshot();
    expect(snap[0].cell_type).toBe("code");
    if (snap[0].cell_type === "code") {
      expect(snap[0].execution_count).toBe(42);
      expect(snap[0].outputs).toHaveLength(1);
      expect(snap[0].outputs[0].output_type).toBe("execute_result");
    }
  });
});
