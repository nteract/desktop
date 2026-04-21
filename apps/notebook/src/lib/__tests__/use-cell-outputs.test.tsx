import { renderHook, act } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vite-plus/test";
import type { JupyterOutput } from "../../types";
import {
  resetNotebookExecutions,
  setCellExecutionPointer,
  setExecution,
} from "../notebook-executions";
import {
  resetNotebookOutputs,
  setOutput,
  useCellOutputs,
} from "../notebook-outputs";

// ---------------------------------------------------------------------------
// useCellOutputs — Phase C-lite acceptance tests.
//
// The hook chains cell_id -> execution_id -> output_ids -> outputs store.
// These tests verify:
//   1. Returns a stable empty-array reference when no execution exists.
//   2. Resolves outputs in emission order once wired up.
//   3. Re-derives the list when an output changes, preserving unchanged refs.
// ---------------------------------------------------------------------------

afterEach(() => {
  resetNotebookOutputs();
  resetNotebookExecutions();
});

function streamOutput(text: string): JupyterOutput {
  return { output_type: "stream", name: "stdout", text };
}

describe("useCellOutputs", () => {
  it("returns a stable empty array when the cell has no execution", () => {
    const { result, rerender } = renderHook(() => useCellOutputs("cell-1"));
    const first = result.current;
    rerender();
    expect(result.current).toBe(first);
    expect(first).toHaveLength(0);
  });

  it("resolves outputs in emission order after wiring the execution", () => {
    const { result } = renderHook(() => useCellOutputs("cell-1"));
    const a = streamOutput("a");
    const b = streamOutput("b");

    act(() => {
      setOutput("out-a", a);
      setOutput("out-b", b);
      setExecution("exec-1", {
        cell_id: "cell-1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: ["out-a", "out-b"],
      });
      setCellExecutionPointer("cell-1", "exec-1");
    });

    expect(result.current).toEqual([a, b]);
  });

  it("re-derives when an output changes but keeps unchanged refs identical", () => {
    const a = streamOutput("a");
    const b = streamOutput("b");

    act(() => {
      setOutput("out-a", a);
      setOutput("out-b", b);
      setExecution("exec-1", {
        cell_id: "cell-1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: ["out-a", "out-b"],
      });
      setCellExecutionPointer("cell-1", "exec-1");
    });

    const { result } = renderHook(() => useCellOutputs("cell-1"));
    const first = result.current;
    expect(first).toEqual([a, b]);

    const b2 = streamOutput("b-updated");
    act(() => {
      setOutput("out-b", b2);
    });

    const second = result.current;
    expect(second).not.toBe(first);
    expect(second[0]).toBe(a); // unchanged ref preserved
    expect(second[1]).toBe(b2); // swapped ref
  });

  it("drops outputs immediately when the execution's output_ids are cleared", () => {
    const a = streamOutput("a");
    act(() => {
      setOutput("out-a", a);
      setExecution("exec-1", {
        cell_id: "cell-1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: ["out-a"],
      });
      setCellExecutionPointer("cell-1", "exec-1");
    });

    const { result } = renderHook(() => useCellOutputs("cell-1"));
    expect(result.current).toEqual([a]);

    // Simulate clearOutputsFromDaemon: empty the execution's output_ids
    // while keeping the pointer in place.
    act(() => {
      setExecution("exec-1", {
        cell_id: "cell-1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: [],
      });
    });

    expect(result.current).toHaveLength(0);
  });

  it("returns the empty singleton when execution exists with no outputs", () => {
    act(() => {
      setExecution("exec-1", {
        cell_id: "cell-1",
        execution_count: null,
        status: "queued",
        success: null,
        output_ids: [],
      });
      setCellExecutionPointer("cell-1", "exec-1");
    });

    const { result: r1 } = renderHook(() => useCellOutputs("cell-1"));
    const { result: r2 } = renderHook(() => useCellOutputs("cell-2"));
    // Same singleton across cells with no outputs.
    expect(r1.current).toBe(r2.current);
  });
});
