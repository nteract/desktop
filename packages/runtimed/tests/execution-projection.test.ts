import { describe, expect, it } from "vite-plus/test";

import {
  buildRuntimeExecutionSnapshot,
  collectExecutionOutputIds,
  collectOutputIds,
  executionFingerprint,
  extractOutputId,
  type ExecutionState,
} from "../src";

describe("execution projection helpers", () => {
  it("extracts only non-empty string output ids", () => {
    expect(extractOutputId({ output_id: "out-1" })).toBe("out-1");
    expect(extractOutputId({ output_id: "" })).toBeNull();
    expect(extractOutputId({ output_id: 123 })).toBeNull();
    expect(extractOutputId(null)).toBeNull();
  });

  it("collects ordered output ids and skips unstamped outputs", () => {
    expect(
      collectOutputIds([
        { output_id: "first" },
        { output_id: "" },
        { output_type: "stream" },
        { output_id: "second" },
      ]),
    ).toEqual(["first", "second"]);
  });

  it("collects execution output ids from RuntimeState entries", () => {
    const entry: ExecutionState = {
      cell_id: "cell-1",
      execution_count: 1,
      status: "running",
      success: null,
      outputs: [{ output_id: "out-1" }, { output_id: "out-2" }],
    };

    expect(collectExecutionOutputIds(entry)).toEqual(["out-1", "out-2"]);
  });

  it("fingerprints same-length output id replacements", () => {
    const base: ExecutionState = {
      cell_id: "cell-1",
      execution_count: 1,
      status: "running",
      success: null,
      outputs: [{ output_id: "old" }],
    };
    const replaced: ExecutionState = {
      ...base,
      outputs: [{ output_id: "new" }],
    };

    expect(executionFingerprint(replaced)).not.toBe(executionFingerprint(base));
  });

  it("derives execution snapshots without carrying raw outputs", () => {
    const entry: ExecutionState = {
      cell_id: "cell-1",
      execution_count: 2,
      status: "done",
      success: true,
      outputs: [{ output_id: "out-1", text: "hello" }],
    };

    expect(buildRuntimeExecutionSnapshot(entry)).toEqual({
      cell_id: "cell-1",
      execution_count: 2,
      status: "done",
      success: true,
      output_ids: ["out-1"],
    });
  });
});
