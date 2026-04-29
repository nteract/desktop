import {
  classifyCellChangesetMaterialization,
  type CellChangeset,
  mergeChangesets,
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
});
