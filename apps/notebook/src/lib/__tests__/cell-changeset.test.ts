import { describe, expect, it } from "vite-plus/test";
import {
  type CellChangeset,
  type ChangedFields,
  mergeChangesets,
} from "../cell-changeset";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const empty: CellChangeset = {
  changed: [],
  added: [],
  removed: [],
  order_changed: false,
};

function sourceOnly(cellId: string): CellChangeset {
  return {
    changed: [{ cell_id: cellId, fields: { source: true } }],
    added: [],
    removed: [],
    order_changed: false,
  };
}

function outputsOnly(cellId: string): CellChangeset {
  return {
    changed: [
      { cell_id: cellId, fields: { outputs: true, execution_count: true } },
    ],
    added: [],
    removed: [],
    order_changed: false,
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CellChangeset helpers", () => {
  describe("mergeChangesets", () => {
    it("merging two empty changesets produces empty", () => {
      const result = mergeChangesets(empty, empty);
      expect(result.changed).toEqual([]);
      expect(result.added).toEqual([]);
      expect(result.removed).toEqual([]);
      expect(result.order_changed).toBe(false);
    });

    it("merging empty with non-empty returns the non-empty", () => {
      const cs = sourceOnly("cell-1");
      const result = mergeChangesets(empty, cs);
      expect(result.changed).toHaveLength(1);
      expect(result.changed[0].cell_id).toBe("cell-1");
      expect(result.changed[0].fields.source).toBe(true);
    });

    it("merges disjoint cell changes", () => {
      const a = sourceOnly("cell-1");
      const b = outputsOnly("cell-2");
      const result = mergeChangesets(a, b);
      expect(result.changed).toHaveLength(2);
      const ids = result.changed.map((c) => c.cell_id).sort();
      expect(ids).toEqual(["cell-1", "cell-2"]);
    });

    it("merges overlapping cell changes with field union", () => {
      const a: CellChangeset = {
        changed: [{ cell_id: "cell-1", fields: { source: true } }],
        added: [],
        removed: [],
        order_changed: false,
      };
      const b: CellChangeset = {
        changed: [
          {
            cell_id: "cell-1",
            fields: { outputs: true, execution_count: true },
          },
        ],
        added: [],
        removed: [],
        order_changed: false,
      };
      const result = mergeChangesets(a, b);
      expect(result.changed).toHaveLength(1);
      const fields = result.changed[0].fields;
      expect(fields.source).toBe(true);
      expect(fields.outputs).toBe(true);
      expect(fields.execution_count).toBe(true);
      expect(fields.metadata).toBeUndefined();
    });

    it("deduplicates added cell IDs", () => {
      const a: CellChangeset = {
        ...empty,
        added: ["cell-new"],
        order_changed: true,
      };
      const b: CellChangeset = {
        ...empty,
        added: ["cell-new", "cell-other"],
        order_changed: true,
      };
      const result = mergeChangesets(a, b);
      expect(result.added).toEqual(
        expect.arrayContaining(["cell-new", "cell-other"]),
      );
      // No duplicate cell-new
      expect(result.added.filter((id) => id === "cell-new")).toHaveLength(1);
    });

    it("deduplicates removed cell IDs", () => {
      const a: CellChangeset = { ...empty, removed: ["cell-x"] };
      const b: CellChangeset = { ...empty, removed: ["cell-x", "cell-y"] };
      const result = mergeChangesets(a, b);
      expect(result.removed.filter((id) => id === "cell-x")).toHaveLength(1);
      expect(result.removed).toContain("cell-y");
    });

    it("propagates order_changed from either side", () => {
      const a: CellChangeset = { ...empty, order_changed: true };
      const b: CellChangeset = { ...empty, order_changed: false };
      expect(mergeChangesets(a, b).order_changed).toBe(true);
      expect(mergeChangesets(b, a).order_changed).toBe(true);
      expect(mergeChangesets(b, b).order_changed).toBe(false);
    });

    it("handles three-way merge via chaining", () => {
      const a = sourceOnly("cell-1");
      const b = outputsOnly("cell-1");
      const c: CellChangeset = {
        changed: [{ cell_id: "cell-1", fields: { metadata: true } }],
        added: ["cell-2"],
        removed: [],
        order_changed: true,
      };
      const result = mergeChangesets(mergeChangesets(a, b), c);
      expect(result.changed).toHaveLength(1);
      const fields = result.changed[0].fields;
      expect(fields.source).toBe(true);
      expect(fields.outputs).toBe(true);
      expect(fields.execution_count).toBe(true);
      expect(fields.metadata).toBe(true);
      expect(result.added).toEqual(["cell-2"]);
      expect(result.order_changed).toBe(true);
    });

    it("does not mutate input changesets", () => {
      const a = sourceOnly("cell-1");
      const b = outputsOnly("cell-1");
      const aFieldsBefore = { ...a.changed[0].fields };
      mergeChangesets(a, b);
      expect(a.changed[0].fields).toEqual(aFieldsBefore);
    });
  });
});

describe("CellChangeset classification", () => {
  it("source-only changeset", () => {
    const cs = sourceOnly("cell-1");
    const isSourceOnly =
      cs.added.length === 0 &&
      cs.removed.length === 0 &&
      !cs.order_changed &&
      cs.changed.every((c) => {
        const f = c.fields;
        return (
          f.source &&
          !f.outputs &&
          !f.execution_count &&
          !f.cell_type &&
          !f.metadata &&
          !f.position &&
          !f.resolved_assets
        );
      });
    expect(isSourceOnly).toBe(true);
  });

  it("structural changeset with added cells", () => {
    const cs: CellChangeset = {
      ...empty,
      added: ["cell-new"],
      order_changed: true,
    };
    const isStructural =
      cs.added.length > 0 || cs.removed.length > 0 || cs.order_changed;
    expect(isStructural).toBe(true);
  });

  it("non-structural output change", () => {
    const cs = outputsOnly("cell-1");
    const isStructural =
      cs.added.length > 0 || cs.removed.length > 0 || cs.order_changed;
    expect(isStructural).toBe(false);
  });
});

describe("CellChangeset serde compatibility", () => {
  it("round-trips through JSON matching Rust serde output", () => {
    // Simulate what serde-wasm-bindgen produces for a CellChangeset
    const fromRust = {
      changed: [{ cell_id: "cell-1", fields: { source: true, outputs: true } }],
      added: ["cell-2"],
      removed: ["cell-3"],
      order_changed: true,
    };

    // The frontend reads these as typed objects
    const cs: CellChangeset = fromRust;
    expect(cs.changed[0].cell_id).toBe("cell-1");
    expect(cs.changed[0].fields.source).toBe(true);
    expect(cs.changed[0].fields.outputs).toBe(true);
    // Fields not present in the Rust output are undefined (not false)
    expect(cs.changed[0].fields.metadata).toBeUndefined();
    expect(cs.added).toEqual(["cell-2"]);
    expect(cs.removed).toEqual(["cell-3"]);
    expect(cs.order_changed).toBe(true);
  });

  it("handles empty changeset from Rust", () => {
    const fromRust = {
      changed: [],
      added: [],
      removed: [],
      order_changed: false,
    };
    const cs: CellChangeset = fromRust;
    expect(cs.changed).toHaveLength(0);
    expect(cs.added).toHaveLength(0);
    expect(cs.removed).toHaveLength(0);
    expect(cs.order_changed).toBe(false);
  });

  it("ChangedFields only contains true fields (sparse object)", () => {
    // Rust serializes ChangedFields with skip_serializing_if for false fields.
    // Only true fields are present in the JS object.
    const fromRust = { source: true, outputs: true };
    const fields: ChangedFields = fromRust;
    expect(fields.source).toBe(true);
    expect(fields.outputs).toBe(true);
    expect(fields.execution_count).toBeUndefined();
    expect(fields.cell_type).toBeUndefined();
    expect(fields.metadata).toBeUndefined();
    expect(fields.position).toBeUndefined();
    expect(fields.resolved_assets).toBeUndefined();
  });

  it("mergeChangesets works with sparse Rust-style fields", () => {
    // First frame: source edit
    const frame1: CellChangeset = {
      changed: [{ cell_id: "cell-1", fields: { source: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    // Second frame: output from daemon (no source field at all)
    const frame2: CellChangeset = {
      changed: [
        {
          cell_id: "cell-1",
          fields: { outputs: true, execution_count: true },
        },
      ],
      added: [],
      removed: [],
      order_changed: false,
    };
    const merged = mergeChangesets(frame1, frame2);
    expect(merged.changed).toHaveLength(1);
    expect(merged.changed[0].fields.source).toBe(true);
    expect(merged.changed[0].fields.outputs).toBe(true);
    expect(merged.changed[0].fields.execution_count).toBe(true);
  });
});
