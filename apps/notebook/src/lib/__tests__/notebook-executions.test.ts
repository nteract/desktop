import { afterEach, describe, expect, it } from "vite-plus/test";
import {
  getCellExecutionId,
  getExecutionById,
  resetNotebookExecutions,
  setCellExecutionPointer,
  setExecution,
  useCellExecutionId,
  useExecution,
  type ExecutionSnapshot,
} from "../notebook-executions";

afterEach(() => {
  resetNotebookExecutions();
});

const snap = (overrides: Partial<ExecutionSnapshot> = {}): ExecutionSnapshot => ({
  cell_id: overrides.cell_id ?? "cell-1",
  execution_count: overrides.execution_count ?? 1,
  status: overrides.status ?? "running",
  success: overrides.success ?? null,
  output_ids: overrides.output_ids ?? [],
});

describe("notebook-executions store", () => {
  it("returns undefined for unknown executions", () => {
    expect(getExecutionById("nope")).toBeUndefined();
  });

  it("stores and retrieves executions by id", () => {
    const s = snap();
    setExecution("exec-1", s);
    expect(getExecutionById("exec-1")).toBe(s);
  });

  it("maintains a cell -> execution_id pointer", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    expect(getCellExecutionId("cell-1")).toBe("exec-1");
  });

  it("updates the cell pointer when a fresh execution arrives", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setExecution("exec-2", snap({ cell_id: "cell-1", execution_count: 2 }));
    expect(getCellExecutionId("cell-1")).toBe("exec-2");
  });

  it("is idempotent when writing the same snapshot shape", () => {
    const first = snap({ output_ids: ["o1", "o2"] });
    setExecution("exec-1", first);
    const keptRef = getExecutionById("exec-1");

    // Re-write with structurally-identical data but new reference.
    const second = snap({ output_ids: ["o1", "o2"] });
    setExecution("exec-1", second);
    // `setExecution` short-circuits on equality -- the stored ref stays.
    expect(getExecutionById("exec-1")).toBe(keptRef);
  });

  it("updates when execution_count changes without touching outputs", () => {
    setExecution("exec-1", snap({ execution_count: 1, output_ids: ["o1"] }));
    setExecution("exec-1", snap({ execution_count: 2, output_ids: ["o1"] }));
    const current = getExecutionById("exec-1");
    expect(current?.execution_count).toBe(2);
    expect(current?.output_ids).toEqual(["o1"]);
  });

  it("clears the cell pointer explicitly", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setCellExecutionPointer("cell-1", null);
    expect(getCellExecutionId("cell-1")).toBeNull();
  });

  it("exports hook functions for React integration", () => {
    // Compile-time guard; React hook testing lives in the component suites.
    expect(typeof useExecution).toBe("function");
    expect(typeof useCellExecutionId).toBe("function");
  });

  it("resets the entire store", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setExecution("exec-2", snap({ cell_id: "cell-2" }));
    resetNotebookExecutions();
    expect(getExecutionById("exec-1")).toBeUndefined();
    expect(getExecutionById("exec-2")).toBeUndefined();
    expect(getCellExecutionId("cell-1")).toBeNull();
    expect(getCellExecutionId("cell-2")).toBeNull();
  });
});
