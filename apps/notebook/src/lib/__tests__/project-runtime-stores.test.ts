import { DEFAULT_RUNTIME_STATE, type RuntimeState } from "runtimed";
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

function stateWith(
  executions: RuntimeState["executions"],
): RuntimeState {
  return { ...DEFAULT_RUNTIME_STATE, executions };
}

describe("projectRuntimeStateToExecutions", () => {
  it("captures same-length output_id replacements (e.g. clear_output(wait=True))", () => {
    projectRuntimeStateToExecutions(
      stateWith({
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "running",
          success: null,
          outputs: [
            { output_id: "old", output_type: "stream", name: "stdout", text: "a" },
          ],
        },
      }),
    );
    expect(getExecutionById("exec-1")?.output_ids).toEqual(["old"]);

    // Second tick: same length, different output_id - must not be skipped
    // by the scalar fingerprint.
    projectRuntimeStateToExecutions(
      stateWith({
        "exec-1": {
          cell_id: "cell-1",
          execution_count: 1,
          status: "running",
          success: null,
          outputs: [
            { output_id: "new", output_type: "stream", name: "stdout", text: "b" },
          ],
        },
      }),
    );
    expect(getExecutionById("exec-1")?.output_ids).toEqual(["new"]);
  });

  it("drops outputs with empty output_id from the execution snapshot", () => {
    // Daemon invariant: `create_manifest` (and the error-path fallback in
    // `outputs_to_manifest_refs`) always stamp an `output_id`. If an
    // un-stamped manifest ever reaches the frontend, the projection
    // filters it out rather than inventing a synthetic key.
    projectRuntimeStateToExecutions(
      stateWith({
        "exec-legacy": {
          cell_id: "cell-legacy",
          execution_count: 1,
          status: "done",
          success: true,
          outputs: [
            { output_id: "", output_type: "stream", name: "stdout", text: "x" },
            { output_id: "real-id", output_type: "stream", name: "stdout", text: "y" },
          ],
        },
      }),
    );

    const snap = getExecutionById("exec-legacy");
    expect(snap?.output_ids).toEqual(["real-id"]);
    expect(getOutputById("real-id")).toBeUndefined();
  });

  it("evicts trimmed executions on the next tick", () => {
    projectRuntimeStateToExecutions(
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
    setCellExecutionPointer("cell-1", "exec-1");
    expect(getExecutionById("exec-1")).toBeTruthy();

    // Tick with the execution removed
    projectRuntimeStateToExecutions(stateWith({}));
    expect(getExecutionById("exec-1")).toBeUndefined();
    expect(getCellExecutionId("cell-1")).toBeNull();
  });
});
