import { renderHook, act } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, describe, expect, it } from "vite-plus/test";

// Render count matters here: we want to verify that output mutations do
// NOT cause `useCell` to surface a new snapshot, which would indicate
// chrome re-renders are still coupled to outputs.
import type { JupyterOutput, NotebookCell } from "../../types";
import {
  resetNotebookExecutions,
  setCellExecutionPointer,
  setExecution,
} from "../notebook-executions";
import {
  getCellIdsSnapshot,
  replaceNotebookCells,
  resetNotebookCells,
  useCell,
} from "../notebook-cells";
import {
  resetNotebookOutputs,
  setOutput,
  useCellOutputs,
} from "../notebook-outputs";

// ---------------------------------------------------------------------------
// Phase C-lite acceptance: decoupling cell subscriptions from outputs.
//
// The goal is that an output mutation (stream append, blob resolution, etc.)
// does NOT cause `useCell(cellId)` to return a new NotebookCell reference —
// the cell's chrome (source, execution_count, metadata) is unaffected. Only
// `useCellOutputs(cellId)` re-derives.
// ---------------------------------------------------------------------------

afterEach(() => {
  resetNotebookCells();
  resetNotebookOutputs();
  resetNotebookExecutions();
});

function streamOutput(text: string): JupyterOutput {
  return { output_type: "stream", name: "stdout", text };
}

function codeCell(id: string): NotebookCell {
  return {
    id,
    cell_type: "code",
    source: "print('hi')",
    execution_count: null,
    outputs: [],
    metadata: {},
  };
}

describe("Phase C-lite: cell subscription / outputs decoupling", () => {
  it("output mutations don't change the cell snapshot reference", () => {
    replaceNotebookCells([codeCell("c1")]);
    // Sanity: the cell landed in the store
    expect(getCellIdsSnapshot()).toEqual(["c1"]);

    // Wire an execution + initial output
    act(() => {
      setOutput("o1", streamOutput("a"));
      setExecution("exec-1", {
        cell_id: "c1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: ["o1"],
      });
      setCellExecutionPointer("c1", "exec-1");
    });

    // Subscribe with a render counter. If the cell snapshot changed on
    // output mutations, the hook would re-run and the ref would increment.
    const { result } = renderHook(() => {
      const renderCount = useRef(0);
      renderCount.current += 1;
      const cell = useCell("c1");
      const outputs = useCellOutputs("c1");
      return { renderCount, cell, outputs };
    });

    const initialCell = result.current.cell;
    const initialOutputs = result.current.outputs;
    expect(initialCell?.id).toBe("c1");
    expect(initialOutputs).toHaveLength(1);

    // Mutate the output. The cell snapshot must NOT change — only the
    // outputs derivation should advance.
    act(() => {
      setOutput("o1", streamOutput("a-appended"));
    });

    expect(result.current.cell).toBe(initialCell);
    expect(result.current.outputs).not.toBe(initialOutputs);
    expect((result.current.outputs[0] as { text: string }).text).toBe(
      "a-appended",
    );

    // Add another output — still no cell snapshot change.
    act(() => {
      setOutput("o2", streamOutput("b"));
      setExecution("exec-1", {
        cell_id: "c1",
        execution_count: 1,
        status: "running",
        success: null,
        output_ids: ["o1", "o2"],
      });
    });

    expect(result.current.cell).toBe(initialCell);
    expect(result.current.outputs).toHaveLength(2);
  });
});
