import { afterEach, describe, expect, it } from "vite-plus/test";
import {
  getCellExecutionId,
  getExecutionById,
  resetNotebookExecutions,
  setCellExecutionPointer,
} from "../notebook-executions";
import {
  getOutputById,
  resetNotebookOutputs,
} from "../notebook-outputs";
import { projectRuntimeStateToExecutions } from "../project-runtime-stores";

afterEach(() => {
  resetNotebookExecutions();
  resetNotebookOutputs();
});

describe("projectRuntimeStateToExecutions", () => {
  it("captures same-length output_id replacements (e.g. clear_output(wait=True))", () => {
    // First tick: single output with id "old"
    projectRuntimeStateToExecutions({
      executions: {
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "running",
          success: null,
          outputs: [
            { output_id: "old", output_type: "stream", name: "stdout", text: "a" },
          ],
        },
      },
    });
    expect(getExecutionById("exec-1")?.output_ids).toEqual(["old"]);

    // Second tick: same length, different output_id - must not be skipped
    // by the scalar fingerprint.
    projectRuntimeStateToExecutions({
      executions: {
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "running",
          success: null,
          outputs: [
            { output_id: "new", output_type: "stream", name: "stdout", text: "b" },
          ],
        },
      },
    });
    expect(getExecutionById("exec-1")?.output_ids).toEqual(["new"]);
  });

  it("seeds the outputs store for legacy outputs without a non-empty output_id", () => {
    const rawOutput = {
      output_id: "",
      output_type: "stream",
      name: "stdout",
      text: "legacy output",
    };
    projectRuntimeStateToExecutions({
      executions: {
        "exec-legacy": {
          cell_id: "cell-legacy",
          execution_count: 1,
          status: "done",
          success: true,
          outputs: [rawOutput],
        },
      },
    });

    const snap = getExecutionById("exec-legacy");
    expect(snap?.output_ids).toEqual(["legacy:exec-legacy:0"]);
    const stored = getOutputById("legacy:exec-legacy:0");
    expect(stored).toBeTruthy();
    expect((stored as { text: string }).text).toBe("legacy output");
  });

  it("produces distinct synthesized ids for multiple empty-id outputs", () => {
    const a = {
      output_id: "",
      output_type: "stream",
      name: "stdout",
      text: "alpha",
    };
    const b = {
      output_id: "",
      output_type: "stream",
      name: "stdout",
      text: "beta",
    };
    projectRuntimeStateToExecutions({
      executions: {
        "exec-x": {
          cell_id: "c",
          execution_count: 1,
          status: "done",
          success: true,
          outputs: [a, b],
        },
      },
    });
    const snap = getExecutionById("exec-x");
    expect(snap?.output_ids).toEqual(["legacy:exec-x:0", "legacy:exec-x:1"]);
    expect((getOutputById("legacy:exec-x:0") as { text: string }).text).toBe(
      "alpha",
    );
    expect((getOutputById("legacy:exec-x:1") as { text: string }).text).toBe(
      "beta",
    );
  });

  it("evicts trimmed executions on the next tick", () => {
    projectRuntimeStateToExecutions({
      executions: {
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "done",
          success: true,
          outputs: [],
        },
      },
    });
    setCellExecutionPointer("cell-1", "exec-1");
    expect(getExecutionById("exec-1")).toBeTruthy();

    // Tick with the execution removed
    projectRuntimeStateToExecutions({ executions: {} });
    expect(getExecutionById("exec-1")).toBeUndefined();
    expect(getCellExecutionId("cell-1")).toBeNull();
  });
});
