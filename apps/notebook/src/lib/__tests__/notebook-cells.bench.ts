import { bench, describe } from "vite-plus/test";
import type { JupyterOutput, NotebookCell } from "../../types";
import {
  getCellById,
  getNotebookCellsSnapshot,
  replaceNotebookCells,
  resetNotebookCells,
  updateCellById,
  updateNotebookCells,
} from "../notebook-cells";

// ── Test data generators ──────────────────────────────────────────────

function generateSource(lineCount: number): string {
  const lines: string[] = [];
  for (let i = 0; i < lineCount; i++) {
    if (i === 0) lines.push("import pandas as pd");
    else if (i === 1) lines.push("import numpy as np");
    else if (i % 10 === 0) lines.push(`\n# Section ${i}`);
    else if (i % 5 === 0)
      lines.push(`df_${i} = pd.DataFrame({"col": np.random.randn(${i * 10})})`);
    else
      lines.push(
        `x_${i} = np.array([${Array.from({ length: 20 }, (_, j) => j + i).join(", ")}])`,
      );
  }
  return lines.join("\n");
}

function generateOutput(index: number): JupyterOutput {
  return {
    output_type: "execute_result",
    data: {
      "text/plain": `<DataFrame: ${index * 100} rows × 5 columns>`,
      "text/html": `<div><style scoped>.dataframe { border-collapse: collapse; }</style><table class="dataframe"><thead><tr><th></th><th>col_a</th><th>col_b</th></tr></thead><tbody>${Array.from({ length: 5 }, (_, r) => `<tr><td>${r}</td><td>${(r * 1.5).toFixed(2)}</td><td>${(r * 2.3).toFixed(2)}</td></tr>`).join("")}</tbody></table></div>`,
    },
    execution_count: index + 1,
  };
}

function generateCodeCell(index: number, sourceLines = 50): NotebookCell {
  return {
    cell_type: "code",
    id: `cell-${index}`,
    source: generateSource(sourceLines),
    execution_count: index + 1,
    outputs: index % 3 === 0 ? [generateOutput(index)] : [],
    metadata: { collapsed: false, scrolled: index % 2 === 0 },
  };
}

function generateMarkdownCell(index: number): NotebookCell {
  return {
    cell_type: "markdown",
    id: `cell-${index}`,
    source: `# Heading ${index}\n\nSome text about section ${index}.\n\n- Point one\n- Point two`,
    metadata: {},
  };
}

function generateNotebook(n: number, sourceLines = 50): NotebookCell[] {
  return Array.from({ length: n }, (_, i) =>
    i % 10 < 7 ? generateCodeCell(i, sourceLines) : generateMarkdownCell(i),
  );
}

// ── Benchmarks: replaceNotebookCells ──────────────────────────────────

describe("replaceNotebookCells", () => {
  for (const count of [10, 50, 100, 500]) {
    const cells = generateNotebook(count);

    bench(
      `replace ${count} cells`,
      () => {
        replaceNotebookCells(cells);
      },
      { teardown: () => resetNotebookCells() },
    );
  }
});

// ── Benchmarks: updateCellById (the typing hot path) ─────────────────
//
// Each iteration starts from a pre-populated store via setup, then
// measures only the targeted single-cell update.

describe("updateCellById — typing hot path", () => {
  for (const count of [10, 50, 100, 500]) {
    const cells = generateNotebook(count);

    bench(
      `updateCellById in ${count}-cell notebook`,
      () => {
        updateCellById("cell-0", (c) => ({ ...c, source: `${c.source}x` }));
      },
      {
        setup: () => replaceNotebookCells(cells),
        teardown: () => resetNotebookCells(),
      },
    );
  }
});

// ── Benchmarks: updateNotebookCells (old approach, for comparison) ────
//
// The pre-split approach: map over ALL cells to update one.

describe("updateNotebookCells — full-array update", () => {
  for (const count of [10, 50, 100, 500]) {
    const cells = generateNotebook(count);

    bench(
      `updateNotebookCells single-cell edit in ${count}-cell notebook`,
      () => {
        updateNotebookCells((prev) =>
          prev.map((c) =>
            c.id === "cell-0" ? { ...c, source: `${c.source}x` } : c,
          ),
        );
      },
      {
        setup: () => replaceNotebookCells(cells),
        teardown: () => resetNotebookCells(),
      },
    );
  }
});

// ── Benchmarks: getNotebookCellsSnapshot ──────────────────────────────

describe("getNotebookCellsSnapshot", () => {
  for (const count of [10, 50, 100, 500]) {
    const cells = generateNotebook(count);

    bench(
      `snapshot read of ${count} cells`,
      () => {
        getNotebookCellsSnapshot();
      },
      {
        setup: () => replaceNotebookCells(cells),
        teardown: () => resetNotebookCells(),
      },
    );
  }
});

// ── Benchmarks: getCellById (O(1) map lookup) ─────────────────────────

describe("getCellById", () => {
  for (const count of [10, 50, 100, 500]) {
    const cells = generateNotebook(count);

    bench(
      `getCellById in ${count}-cell notebook`,
      () => {
        getCellById("cell-0");
      },
      {
        setup: () => replaceNotebookCells(cells),
        teardown: () => resetNotebookCells(),
      },
    );
  }
});

// ── Benchmarks: updateCellById vs updateNotebookCells ─────────────────
//
// Direct comparison at the same notebook size, measuring only the update.

describe("targeted vs full-array update (100 cells)", () => {
  const cells = generateNotebook(100);

  bench(
    "updateCellById (targeted, O(1) notify)",
    () => {
      updateCellById("cell-0", (c) => ({ ...c, source: `${c.source}x` }));
    },
    {
      setup: () => replaceNotebookCells(cells),
      teardown: () => resetNotebookCells(),
    },
  );

  bench(
    "updateNotebookCells (full array map + diff)",
    () => {
      updateNotebookCells((prev) =>
        prev.map((c) =>
          c.id === "cell-0" ? { ...c, source: `${c.source}x` } : c,
        ),
      );
    },
    {
      setup: () => replaceNotebookCells(cells),
      teardown: () => resetNotebookCells(),
    },
  );
});
