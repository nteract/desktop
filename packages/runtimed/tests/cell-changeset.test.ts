import {
  classifyCellChangesetMaterialization,
  cellChangesetTouchesChrome,
  type CellChangeset,
  mergeChangesets,
  planCellChangesetProjection,
  summarizeChangedFields,
} from "runtimed";
import { describe, expect, it } from "vite-plus/test";

const empty: CellChangeset = {
  changed: [],
  added: [],
  removed: [],
  order_changed: false,
};

function outputsOnly(cellId: string): CellChangeset {
  return {
    changed: [{ cell_id: cellId, fields: { outputs: true } }],
    added: [],
    removed: [],
    order_changed: false,
  };
}

describe("CellChangeset helpers", () => {
  it("merges sparse changed fields without mutating inputs", () => {
    const source: CellChangeset = {
      changed: [{ cell_id: "cell-1", fields: { source: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const outputs = outputsOnly("cell-1");
    const originalSourceFields = { ...source.changed[0].fields };

    expect(mergeChangesets(source, outputs)).toEqual({
      changed: [{ cell_id: "cell-1", fields: { source: true, outputs: true } }],
      added: [],
      removed: [],
      order_changed: false,
    });
    expect(source.changed[0].fields).toEqual(originalSourceFields);
  });

  it("classifies missing or structural changesets as full materialization", () => {
    expect(classifyCellChangesetMaterialization(null)).toEqual({
      kind: "full",
      reason: "missing_changeset",
    });
    expect(
      classifyCellChangesetMaterialization({
        ...empty,
        added: ["cell-new"],
      }),
    ).toEqual({ kind: "full", reason: "structural" });
    expect(
      classifyCellChangesetMaterialization({
        ...empty,
        changed: [{ cell_id: "cell-1", fields: { resolved_assets: true } }],
      }),
    ).toEqual({ kind: "full", reason: "resolved_assets" });
  });

  it("classifies non-structural cell updates as incremental materialization", () => {
    expect(classifyCellChangesetMaterialization(outputsOnly("cell-1"))).toEqual({
      kind: "incremental",
    });
  });

  it("plans output-only updates without cell chrome writes", () => {
    expect(planCellChangesetProjection(outputsOnly("cell-1"))).toEqual({
      kind: "incremental",
      cells: [
        {
          cell_id: "cell-1",
          fields: { outputs: true },
          touches_chrome: false,
          touches_outputs: true,
          preserve_source: true,
          field_summary: ["out"],
        },
      ],
    });
  });

  it("plans chrome updates and source preservation for app projections", () => {
    expect(
      planCellChangesetProjection({
        ...empty,
        changed: [
          {
            cell_id: "cell-1",
            fields: { outputs: true, execution_count: true },
          },
          {
            cell_id: "cell-2",
            fields: { source: true, metadata: true },
          },
        ],
      }),
    ).toEqual({
      kind: "incremental",
      cells: [
        {
          cell_id: "cell-1",
          fields: { outputs: true, execution_count: true },
          touches_chrome: true,
          touches_outputs: true,
          preserve_source: true,
          field_summary: ["out", "ec"],
        },
        {
          cell_id: "cell-2",
          fields: { source: true, metadata: true },
          touches_chrome: true,
          touches_outputs: false,
          preserve_source: false,
          field_summary: ["src", "meta"],
        },
      ],
    });
  });

  it("exposes field-level projection helpers", () => {
    expect(cellChangesetTouchesChrome({ outputs: true })).toBe(false);
    expect(cellChangesetTouchesChrome({ position: true })).toBe(true);
    expect(
      summarizeChangedFields({
        source: true,
        outputs: true,
        execution_count: true,
        metadata: true,
        position: true,
      }),
    ).toEqual(["src", "out", "ec", "meta"]);
  });
});
