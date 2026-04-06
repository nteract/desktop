import { bench, describe } from "vitest";
import type { JupyterOutput } from "../../types";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCellsSync,
} from "../materialize-cells";

// ── Test data generators ──────────────────────────────────────────────

function generateSource(lineCount: number): string {
  const lines: string[] = [];
  for (let i = 0; i < lineCount; i++) {
    lines.push(`x_${i} = ${i} * 2  # computation line ${i}`);
  }
  return lines.join("\n");
}

function generateOutputManifest(index: number): Record<string, unknown> {
  return {
    output_type: "execute_result",
    data: {
      "text/plain": { inline: `Result ${index}` },
      "text/html": {
        inline: `<div class="output"><table>${Array.from({ length: 10 }, (_, r) => `<tr><td>${r}</td><td>${(Math.random() * 100).toFixed(2)}</td></tr>`).join("")}</table></div>`,
      },
    },
    execution_count: index + 1,
    metadata: {},
  };
}

function generateCellSnapshot(
  index: number,
  sourceLines = 50,
  withOutputs = true,
): CellSnapshot {
  const isCode = index % 10 < 7;
  return {
    id: `cell-${index}`,
    cell_type: isCode ? "code" : "markdown",
    position: (0x80 + index).toString(16),
    source: generateSource(sourceLines),
    execution_count: isCode ? `${index + 1}` : "null",
    outputs:
      isCode && withOutputs && index % 3 === 0
        ? [generateOutputManifest(index)]
        : [],
    metadata: { collapsed: false },
  };
}

function generateSnapshots(count: number, sourceLines = 50): CellSnapshot[] {
  return Array.from({ length: count }, (_, i) =>
    generateCellSnapshot(i, sourceLines),
  );
}

// ── Benchmarks: cellSnapshotsToNotebookCellsSync ──────────────────────

describe("cellSnapshotsToNotebookCellsSync", () => {
  for (const count of [10, 50, 100, 500]) {
    const snapshots = generateSnapshots(count);
    const cache = new Map<string, JupyterOutput>();

    bench(`materialize ${count} cells (cold cache)`, () => {
      const freshCache = new Map<string, JupyterOutput>();
      cellSnapshotsToNotebookCellsSync(snapshots, freshCache);
    });

    // Warm the cache first
    cellSnapshotsToNotebookCellsSync(snapshots, cache);

    bench(`materialize ${count} cells (warm cache)`, () => {
      cellSnapshotsToNotebookCellsSync(snapshots, cache);
    });
  }
});

// ── Benchmarks: JSON.parse baseline (what get_cells_json costs on JS side)

describe("JSON.parse baseline", () => {
  for (const count of [10, 50, 100, 500]) {
    const snapshots = generateSnapshots(count);
    const jsonString = JSON.stringify(snapshots);
    const jsonSize = new Blob([jsonString]).size;

    bench(
      `JSON.parse ${count} cells (${(jsonSize / 1024).toFixed(0)}KB)`,
      () => {
        JSON.parse(jsonString);
      },
    );
  }

  // Also benchmark JSON.stringify (what WASM get_cells_json does internally)
  for (const count of [10, 50, 100, 500]) {
    const snapshots = generateSnapshots(count);

    bench(`JSON.stringify ${count} cells`, () => {
      JSON.stringify(snapshots);
    });
  }
});

// ── Benchmarks: output cache key computation + lookups ────────────────

describe("per-output cache key + lookup", () => {
  const outputManifests = Array.from({ length: 100 }, (_, i) =>
    generateOutputManifest(i),
  );

  bench("compute 100 output cache keys (JSON.stringify)", () => {
    for (const manifest of outputManifests) {
      JSON.stringify(manifest);
    }
  });

  const cachedOutputs = new Map<string, JupyterOutput>();
  const keys: string[] = [];
  for (const manifest of outputManifests) {
    const key = JSON.stringify(manifest);
    keys.push(key);
    cachedOutputs.set(key, manifest as unknown as JupyterOutput);
  }

  bench("100 output cache lookups (hit)", () => {
    for (const key of keys) {
      cachedOutputs.get(key);
    }
  });
});

// ── Benchmarks: source-only materialization cost ──────────────────────
//
// This approximates what an incremental "only re-read changed cells" path
// would cost vs full materialization. Useful as a baseline to compare against
// future WASM per-cell accessor performance.

describe("incremental vs full materialization cost", () => {
  const snapshots = generateSnapshots(100);
  const cache = new Map<string, JupyterOutput>();
  // Warm cache
  cellSnapshotsToNotebookCellsSync(snapshots, cache);

  bench("full materialization (100 cells, warm cache)", () => {
    cellSnapshotsToNotebookCellsSync(snapshots, cache);
  });

  // Simulate incremental: only materialize 1 changed cell
  const singleSnapshot = [snapshots[0]];

  bench("single-cell materialization (1 of 100, warm cache)", () => {
    cellSnapshotsToNotebookCellsSync(singleSnapshot, cache);
  });

  // Simulate incremental: materialize 5 changed cells (agent editing multiple)
  const fiveSnapshots = snapshots.slice(0, 5);

  bench("5-cell materialization (5 of 100, warm cache)", () => {
    cellSnapshotsToNotebookCellsSync(fiveSnapshots, cache);
  });
});
