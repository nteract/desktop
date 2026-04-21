import { afterEach, describe, expect, it } from "vite-plus/test";
import {
  deleteExecutions,
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

  it("does NOT auto-update the cell pointer from setExecution", () => {
    // setExecution keeps per-eid snapshots only. The cell pointer is
    // driven separately by setCellExecutionPointer (canonical source is
    // the notebook doc's cells.{id}.execution_id field).
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    expect(getCellExecutionId("cell-1")).toBeNull();
  });

  it("respects explicit cell pointer updates", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setCellExecutionPointer("cell-1", "exec-1");
    setExecution("exec-2", snap({ cell_id: "cell-1", execution_count: 2 }));
    setCellExecutionPointer("cell-1", "exec-2");
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
    setCellExecutionPointer("cell-1", "exec-1");
    setCellExecutionPointer("cell-1", null);
    expect(getCellExecutionId("cell-1")).toBeNull();
  });

  it("evicts executions and clears matching cell pointers", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setExecution("exec-2", snap({ cell_id: "cell-2" }));
    setCellExecutionPointer("cell-1", "exec-1");
    setCellExecutionPointer("cell-2", "exec-2");
    deleteExecutions(["exec-1"]);
    expect(getExecutionById("exec-1")).toBeUndefined();
    expect(getCellExecutionId("cell-1")).toBeNull();
    // Other cell's pointer survives.
    expect(getCellExecutionId("cell-2")).toBe("exec-2");
  });

  it("leaves the cell pointer alone when evicting an older execution", () => {
    setExecution("exec-1", snap({ cell_id: "cell-1" }));
    setExecution("exec-2", snap({ cell_id: "cell-1", execution_count: 2 }));
    // Pointer is at the latest execution.
    setCellExecutionPointer("cell-1", "exec-2");
    // Dropping the older (non-current) execution must not clear the pointer.
    deleteExecutions(["exec-1"]);
    expect(getCellExecutionId("cell-1")).toBe("exec-2");
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
