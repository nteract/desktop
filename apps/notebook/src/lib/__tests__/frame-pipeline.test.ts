import { beforeEach, describe, expect, it, vi } from "vitest";
import type { JupyterOutput } from "../../types";
import {
  type CellChangeset,
  type ChangedFields,
  mergeChangesets,
} from "../cell-changeset";
import { type MaterializeDeps, materializeChangeset } from "../frame-pipeline";

// ── Mocks ──────────────────────────────────────────────────────────────

// Mock notebook-cells: getCellById returns a stored cell, updateCellById
// captures what the pipeline writes to the store.
const cellStore = new Map<string, Record<string, unknown>>();
const updateCalls: Array<{ cellId: string; cell: Record<string, unknown> }> =
  [];

vi.mock("../notebook-cells", () => ({
  getCellById: (id: string) => cellStore.get(id) ?? null,
  updateCellById: (
    id: string,
    fn: (prev: Record<string, unknown>) => Record<string, unknown>,
  ) => {
    const prev = cellStore.get(id) ?? {};
    const next = fn(prev);
    cellStore.set(id, next);
    updateCalls.push({ cellId: id, cell: next });
  },
}));

vi.mock("../notebook-metadata", () => ({
  notifyMetadataChanged: vi.fn(),
}));

vi.mock("../blob-port", () => ({
  getBlobPort: () => 12345,
  refreshBlobPort: () => Promise.resolve(12345),
}));

vi.mock("../logger", () => ({
  logger: { debug: vi.fn(), info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

// Mock materialize-cells: materializeCellFromWasm builds a cell from the
// handle's per-field accessors. isManifestHash and resolveOutput handle
// the cache-miss path.
vi.mock("../materialize-cells", () => ({
  isManifestHash: (s: string) => s.startsWith("sha256:"),
  materializeCellFromWasm: (
    handle: Record<string, (id: string) => unknown>,
    cellId: string,
    cache: Map<string, JupyterOutput>,
    _prev?: Record<string, unknown>,
  ) => {
    const cellType = handle.get_cell_type(cellId);
    if (!cellType) return null;
    const source = (handle.get_cell_source(cellId) as string) ?? "";
    const ecStr = handle.get_cell_execution_count(cellId) as string;
    const ec = !ecStr || ecStr === "null" ? null : Number.parseInt(ecStr, 10);
    const rawOutputs = (handle.get_cell_outputs(cellId) as string[]) ?? [];
    const outputs = rawOutputs
      .map((o: string) => cache.get(o) ?? null)
      .filter((o: JupyterOutput | null): o is JupyterOutput => o !== null);
    const metadata =
      (handle.get_cell_metadata(cellId) as Record<string, unknown>) ?? {};
    return {
      id: cellId,
      cell_type: cellType,
      source,
      execution_count: ec,
      outputs,
      metadata,
    };
  },
  resolveOutput: async (
    outputStr: string,
    _blobPort: number,
    cache: Map<string, JupyterOutput>,
  ) => {
    // Simulate async blob resolution — check cache, test-level registry,
    // or parse JSON.
    const cached = cache.get(outputStr);
    if (cached) return cached;
    // Check test-level registry for sha256 hashes
    const registered = blobRegistry.get(outputStr);
    if (registered) {
      cache.set(outputStr, registered);
      return registered;
    }
    try {
      const parsed = JSON.parse(outputStr) as JupyterOutput;
      cache.set(outputStr, parsed);
      return parsed;
    } catch {
      return null;
    }
  },
}));

// Test-level registry for simulating blob server responses for sha256 hashes.
const blobRegistry = new Map<string, JupyterOutput>();

// ---------------------------------------------------------------------------
// mergeChangesets — merges two CellChangesets produced by successive WASM
// sync frames within a coalescing window.
// ---------------------------------------------------------------------------

describe("mergeChangesets", () => {
  const empty: CellChangeset = {
    changed: [],
    added: [],
    removed: [],
    order_changed: false,
  };

  it("merges two empty changesets", () => {
    const result = mergeChangesets(empty, empty);
    expect(result).toEqual(empty);
  });

  it("merges when first changeset is empty", () => {
    const b: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true } }],
      added: ["c2"],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(empty, b);
    expect(result.changed).toEqual([
      { cell_id: "c1", fields: { source: true } },
    ]);
    expect(result.added).toEqual(["c2"]);
  });

  it("merges when second changeset is empty", () => {
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { outputs: true } }],
      added: [],
      removed: ["c3"],
      order_changed: true,
    };
    const result = mergeChangesets(a, empty);
    expect(result.changed).toEqual([
      { cell_id: "c1", fields: { outputs: true } },
    ]);
    expect(result.removed).toEqual(["c3"]);
    expect(result.order_changed).toBe(true);
  });

  it("unions changed fields for the same cell_id", () => {
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [
        { cell_id: "c1", fields: { outputs: true, execution_count: true } },
      ],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.changed).toHaveLength(1);
    expect(result.changed[0].cell_id).toBe("c1");
    expect(result.changed[0].fields).toEqual({
      source: true,
      outputs: true,
      execution_count: true,
    });
  });

  it("keeps distinct cells separate", () => {
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [{ cell_id: "c2", fields: { metadata: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.changed).toHaveLength(2);
    const ids = result.changed.map((c) => c.cell_id).sort();
    expect(ids).toEqual(["c1", "c2"]);
  });

  it("deduplicates added cell IDs", () => {
    const a: CellChangeset = {
      changed: [],
      added: ["c1", "c2"],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [],
      added: ["c2", "c3"],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.added).toEqual(["c1", "c2", "c3"]);
  });

  it("deduplicates removed cell IDs", () => {
    const a: CellChangeset = {
      changed: [],
      added: [],
      removed: ["c1"],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [],
      added: [],
      removed: ["c1", "c2"],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.removed).toEqual(["c1", "c2"]);
  });

  it("propagates order_changed if either is true", () => {
    const a: CellChangeset = {
      changed: [],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [],
      added: [],
      removed: [],
      order_changed: true,
    };
    expect(mergeChangesets(a, b).order_changed).toBe(true);
    expect(mergeChangesets(b, a).order_changed).toBe(true);
    expect(mergeChangesets(b, b).order_changed).toBe(true);
    expect(mergeChangesets(a, a).order_changed).toBe(false);
  });

  it("does not set false fields when merging overlapping field keys", () => {
    // If both changesets mark the same field as true, result should be true.
    // If only one does, the field should still be true (union, not intersection).
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true, outputs: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true, metadata: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    const fields = result.changed[0].fields;
    expect(fields.source).toBe(true);
    expect(fields.outputs).toBe(true);
    expect(fields.metadata).toBe(true);
  });

  it("handles all ChangedFields keys", () => {
    const allFields: ChangedFields = {
      source: true,
      outputs: true,
      execution_count: true,
      cell_type: true,
      metadata: true,
      position: true,
      resolved_assets: true,
    };
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true, outputs: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [
        {
          cell_id: "c1",
          fields: {
            execution_count: true,
            cell_type: true,
            metadata: true,
            position: true,
            resolved_assets: true,
          },
        },
      ],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.changed[0].fields).toEqual(allFields);
  });

  it("does not mutate input changesets", () => {
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true } }],
      added: ["c2"],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { outputs: true } }],
      added: ["c3"],
      removed: ["c4"],
      order_changed: true,
    };

    // Snapshot originals
    const aJson = JSON.stringify(a);
    const bJson = JSON.stringify(b);

    mergeChangesets(a, b);

    expect(JSON.stringify(a)).toBe(aJson);
    expect(JSON.stringify(b)).toBe(bJson);
  });

  it("handles many cells across multiple merges (chained)", () => {
    let acc = empty;
    for (let i = 0; i < 100; i++) {
      const cs: CellChangeset = {
        changed: [{ cell_id: `c${i}`, fields: { source: true } }],
        added: i % 10 === 0 ? [`new-${i}`] : [],
        removed: [],
        order_changed: false,
      };
      acc = mergeChangesets(acc, cs);
    }
    expect(acc.changed).toHaveLength(100);
    expect(acc.added).toHaveLength(10);
  });
});

// ---------------------------------------------------------------------------
// materializeChangeset — processes coalesced CellChangesets from the
// SyncEngine and writes the resulting cells to the React store.
// ---------------------------------------------------------------------------

describe("materializeChangeset", () => {
  // Mock handle factory
  function createMockHandle(
    cells: Record<
      string,
      {
        type?: string;
        source?: string;
        outputs?: string[];
        execution_count?: string;
        metadata?: Record<string, unknown>;
      }
    >,
  ) {
    return {
      get_cell_type: vi.fn((id: string) => cells[id]?.type ?? "code"),
      get_cell_source: vi.fn((id: string) => cells[id]?.source ?? ""),
      get_cell_outputs: vi.fn((id: string) => cells[id]?.outputs ?? []),
      get_cell_execution_count: vi.fn(
        (id: string) => cells[id]?.execution_count ?? "null",
      ),
      get_cell_metadata: vi.fn((id: string) => cells[id]?.metadata ?? {}),
    } as unknown as import("../../wasm/runtimed-wasm/runtimed_wasm.js").NotebookHandle;
  }

  function createDeps(
    handle: ReturnType<typeof createMockHandle>,
    outputCache?: Map<string, JupyterOutput>,
  ): MaterializeDeps {
    return {
      getHandle: () => handle,
      materializeCells: vi.fn(),
      outputCache: outputCache ?? new Map(),
    };
  }

  beforeEach(() => {
    cellStore.clear();
    updateCalls.length = 0;
    blobRegistry.clear();
  });

  it("clears cell when WASM returns empty outputs", async () => {
    const handle = createMockHandle({
      c1: { outputs: [], execution_count: "null" },
    });
    const deps = createDeps(handle);

    await materializeChangeset(
      {
        changed: [
          { cell_id: "c1", fields: { outputs: true, execution_count: true } },
        ],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(1);
    expect(updateCalls[0].cellId).toBe("c1");
    expect(updateCalls[0].cell).toMatchObject({
      outputs: [],
      execution_count: null,
    });
  });

  it("restores final outputs from WASM via cache", async () => {
    const streamOutput: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello\n",
    };
    const cache = new Map<string, JupyterOutput>();
    cache.set("out-hash-1", streamOutput);

    const handle = createMockHandle({
      c1: { outputs: ["out-hash-1"], execution_count: "5" },
    });
    const deps = createDeps(handle, cache);

    await materializeChangeset(
      {
        changed: [
          { cell_id: "c1", fields: { outputs: true, execution_count: true } },
        ],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(1);
    expect(updateCalls[0].cell).toMatchObject({
      outputs: [streamOutput],
      execution_count: 5,
    });
  });

  it("preserves source when fields.source is false", async () => {
    // Pre-populate store with existing source
    cellStore.set("c1", {
      id: "c1",
      cell_type: "code",
      source: "print('original')",
      outputs: [],
      execution_count: null,
      metadata: {},
    });

    const handle = createMockHandle({
      c1: {
        source: "print('from-wasm')",
        outputs: [],
        execution_count: "3",
      },
    });
    const deps = createDeps(handle);

    await materializeChangeset(
      {
        changed: [
          {
            cell_id: "c1",
            fields: { outputs: true, execution_count: true },
            // source is NOT in fields — should preserve store source
          },
        ],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(1);
    // Source should be the original from the store, not from WASM
    expect(updateCalls[0].cell.source).toBe("print('original')");
  });

  it("restores error output on reconciliation", async () => {
    const errorOutput: JupyterOutput = {
      output_type: "error",
      ename: "ZeroDivisionError",
      evalue: "division by zero",
      traceback: ["Traceback...", "ZeroDivisionError: division by zero"],
    };
    const cache = new Map<string, JupyterOutput>();
    cache.set("err-hash-1", errorOutput);

    const handle = createMockHandle({
      c1: { outputs: ["err-hash-1"], execution_count: "2" },
    });
    const deps = createDeps(handle, cache);

    await materializeChangeset(
      {
        changed: [
          { cell_id: "c1", fields: { outputs: true, execution_count: true } },
        ],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(1);
    expect(updateCalls[0].cell.outputs).toEqual([errorOutput]);
    expect(updateCalls[0].cell.execution_count).toBe(2);
  });

  it("falls back to full materialization for null changeset", async () => {
    const handle = createMockHandle({});
    const deps = createDeps(handle);

    await materializeChangeset(null, deps);

    expect(deps.materializeCells).toHaveBeenCalledWith(handle);
  });

  it("falls back to full materialization for structural changes", async () => {
    const handle = createMockHandle({});
    const deps = createDeps(handle);

    await materializeChangeset(
      {
        changed: [],
        added: ["new-cell"],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(deps.materializeCells).toHaveBeenCalledWith(handle);
  });

  it("resolves uncached manifest hashes via async blob fetch", async () => {
    const execResult: JupyterOutput = {
      output_type: "execute_result",
      data: { "text/plain": "42" },
      metadata: {},
      execution_count: 1,
    };

    // Use a sha256: hash so isManifestHash returns true. The output cache
    // is empty so allCached=false, triggering the async resolution path.
    // The blobRegistry simulates the blob server returning this output.
    const hash = "sha256:abc123";
    blobRegistry.set(hash, execResult);
    const cache = new Map<string, JupyterOutput>();

    const handle = createMockHandle({
      c1: {
        outputs: [hash],
        execution_count: "1",
        source: "6 * 7",
      },
    });
    const deps = createDeps(handle, cache);

    await materializeChangeset(
      {
        changed: [{ cell_id: "c1", fields: { outputs: true, source: true } }],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(1);
    expect(updateCalls[0].cell.outputs).toEqual([execResult]);
    expect(updateCalls[0].cell.source).toBe("6 * 7");
  });

  it("returns early when handle is null", async () => {
    const deps: MaterializeDeps = {
      getHandle: () => null,
      materializeCells: vi.fn(),
      outputCache: new Map(),
    };

    await materializeChangeset(
      {
        changed: [{ cell_id: "c1", fields: { outputs: true } }],
        added: [],
        removed: [],
        order_changed: false,
      },
      deps,
    );

    expect(updateCalls).toHaveLength(0);
    expect(deps.materializeCells).not.toHaveBeenCalled();
  });
});
