import { describe, expect, it } from "vitest";
import {
  type CellChangeset,
  type ChangedFields,
  mergeChangesets,
} from "../cell-changeset";

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
