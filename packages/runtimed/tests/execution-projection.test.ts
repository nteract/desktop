import { describe, expect, it } from "vite-plus/test";

import {
  buildRuntimeExecutionSnapshot,
  collectExecutionOutputIds,
  collectOutputIds,
  DEFAULT_RUNTIME_STATE,
  executionFingerprint,
  extractOutputId,
  type ExecutionState,
  RuntimeExecutionProjector,
  type RuntimeState,
} from "../src";

function stateWith(executions: RuntimeState["executions"]): RuntimeState {
  return { ...DEFAULT_RUNTIME_STATE, executions };
}

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

describe("RuntimeExecutionProjector", () => {
  it("emits upserts for changed executions and skips unchanged ticks", () => {
    const projector = new RuntimeExecutionProjector();
    const state = stateWith({
      "exec-1": {
        cell_id: "cell-1",
        execution_count: 1,
        status: "running",
        success: null,
        outputs: [{ output_id: "out-1" }],
      },
    });

    expect(projector.project(state)).toEqual({
      upserts: [
        [
          "exec-1",
          {
            cell_id: "cell-1",
            execution_count: 1,
            status: "running",
            success: null,
            output_ids: ["out-1"],
          },
        ],
      ],
      removed_execution_ids: [],
    });
    expect(projector.project(state)).toEqual({
      upserts: [],
      removed_execution_ids: [],
    });
  });

  it("captures same-length output_id replacements", () => {
    const projector = new RuntimeExecutionProjector();
    projector.project(
      stateWith({
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "running",
          success: null,
          outputs: [{ output_id: "old", output_type: "stream", text: "a" }],
        },
      }),
    );

    expect(
      projector.project(
        stateWith({
          "exec-1": {
            cell_id: "cell-1",
            execution_count: 1,
            status: "running",
            success: null,
            outputs: [{ output_id: "new", output_type: "stream", text: "b" }],
          },
        }),
      ).upserts[0]?.[1].output_ids,
    ).toEqual(["new"]);
  });

  it("emits removed execution ids for daemon-trimmed entries", () => {
    const projector = new RuntimeExecutionProjector();
    projector.project(
      stateWith({
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "done",
          success: true,
          outputs: [],
        },
      }),
    );

    expect(projector.project(stateWith({}))).toEqual({
      upserts: [],
      removed_execution_ids: ["exec-1"],
    });
    expect(projector.project(stateWith({}))).toEqual({
      upserts: [],
      removed_execution_ids: [],
    });
  });

  it("reset clears projection cache", () => {
    const projector = new RuntimeExecutionProjector();
    const state = stateWith({
      "exec-1": {
        cell_id: "cell-1",
        execution_count: 1,
        status: "done",
        success: true,
        outputs: [],
      },
    });
    projector.project(state);

    projector.reset();

    expect(projector.project(state).upserts).toHaveLength(1);
  });
});
