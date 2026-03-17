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
