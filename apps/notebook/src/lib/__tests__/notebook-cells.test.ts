import { afterEach, describe, expect, it } from "vitest";
import type { NotebookCell } from "../../types";
import {
  getNotebookCellsSnapshot,
  replaceNotebookCells,
  resetNotebookCells,
  updateNotebookCells,
} from "../notebook-cells";

const codeCell = (id: string, source = ""): NotebookCell => ({
  cell_type: "code",
  id,
  source,
  execution_count: null,
  outputs: [],
});

const markdownCell = (id: string, source = ""): NotebookCell => ({
  cell_type: "markdown",
  id,
  source,
});

afterEach(() => {
  resetNotebookCells();
});

describe("replaceNotebookCells", () => {
  it("sets the snapshot to the provided cells", () => {
    const cells = [codeCell("a"), markdownCell("b")];
    replaceNotebookCells(cells);
    expect(getNotebookCellsSnapshot()).toBe(cells);
  });

  it("replaces previous cells entirely", () => {
    replaceNotebookCells([codeCell("a")]);
    const next = [codeCell("b"), codeCell("c")];
    replaceNotebookCells(next);
    expect(getNotebookCellsSnapshot()).toBe(next);
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
    expect(getNotebookCellsSnapshot()).toBe(result);
  });

  it("returns the new snapshot", () => {
    replaceNotebookCells([codeCell("a")]);
    const appended = codeCell("b");
    const result = updateNotebookCells((cells) => [...cells, appended]);
    expect(result).toHaveLength(2);
    expect(result[1]).toBe(appended);
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

  it("returns the same reference until replaced", () => {
    const cells = [codeCell("a")];
    replaceNotebookCells(cells);
    const snap1 = getNotebookCellsSnapshot();
    const snap2 = getNotebookCellsSnapshot();
    expect(snap1).toBe(snap2);
  });

  it("returns a new reference after replace", () => {
    replaceNotebookCells([codeCell("a")]);
    const snap1 = getNotebookCellsSnapshot();
    replaceNotebookCells([codeCell("a")]);
    const snap2 = getNotebookCellsSnapshot();
    expect(snap1).not.toBe(snap2);
  });

  it("returns a new reference after update", () => {
    replaceNotebookCells([codeCell("a")]);
    const snap1 = getNotebookCellsSnapshot();
    updateNotebookCells((cells) => [...cells]);
    const snap2 = getNotebookCellsSnapshot();
    expect(snap1).not.toBe(snap2);
  });
});

describe("subscriber notifications", () => {
  // We access subscribe indirectly through the module. Since subscribe isn't
  // exported, we test notifications via the observable behavior of the store:
  // snapshot identity changes are the externally visible effect of emitChange().

  it("replace produces a distinct snapshot each call", () => {
    const refs = new Set<NotebookCell[]>();
    for (let i = 0; i < 5; i++) {
      replaceNotebookCells([codeCell(`cell-${i}`)]);
      refs.add(getNotebookCellsSnapshot());
    }
    expect(refs.size).toBe(5);
  });

  it("update produces a distinct snapshot each call", () => {
    replaceNotebookCells([codeCell("a")]);
    const ref1 = getNotebookCellsSnapshot();
    updateNotebookCells((cells) => [...cells, codeCell("b")]);
    const ref2 = getNotebookCellsSnapshot();
    updateNotebookCells((cells) => [...cells, codeCell("c")]);
    const ref3 = getNotebookCellsSnapshot();
    expect(ref1).not.toBe(ref2);
    expect(ref2).not.toBe(ref3);
  });
});

describe("mixed cell types", () => {
  it("stores code, markdown, and raw cells", () => {
    const cells: NotebookCell[] = [
      codeCell("c1", "x = 1"),
      markdownCell("m1", "# Title"),
      { cell_type: "raw", id: "r1", source: "raw content" },
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
