import { describe, expect, it } from "vite-plus/test";

import {
  DISPLAY_CAPABLE_JUPYTER_OUTPUT_TYPES,
  INITIAL_LOAD_PHASES,
  NOTEBOOK_DOC_PHASES,
  NOTEBOOK_REQUEST_TYPES,
  NOTEBOOK_RESPONSE_RESULTS,
  RUNTIME_STATE_PHASES,
  SESSION_CONTROL_TYPES,
  isDisplayCapableJupyterOutput,
  isDisplayCapableJupyterOutputType,
  isInitialLoadFailed,
  isInitialLoadStreaming,
  type InitialLoadPhase,
} from "../src";

describe("protocol contract discriminants", () => {
  it("lists notebook request discriminants in wire order", () => {
    expect(NOTEBOOK_REQUEST_TYPES).toEqual([
      "launch_kernel",
      "execute_cell",
      "execute_cell_guarded",
      "interrupt_execution",
      "shutdown_kernel",
      "run_all_cells",
      "run_all_cells_guarded",
      "send_comm",
      "get_history",
      "complete",
      "save_notebook",
      "clone_as_ephemeral",
      "sync_environment",
      "approve_trust",
      "approve_project_environment",
      "get_doc_bytes",
    ]);
  });

  it("lists notebook response result discriminants in wire order", () => {
    expect(NOTEBOOK_RESPONSE_RESULTS).toEqual([
      "kernel_launched",
      "kernel_already_running",
      "cell_queued",
      "interrupt_sent",
      "kernel_shutting_down",
      "no_kernel",
      "guard_rejected",
      "all_cells_queued",
      "notebook_saved",
      "save_error",
      "notebook_cloned",
      "ok",
      "error",
      "history_result",
      "completion_result",
      "sync_environment_complete",
      "sync_environment_failed",
      "doc_bytes",
    ]);
  });

  it("lists session-control readiness discriminants", () => {
    expect(SESSION_CONTROL_TYPES).toEqual(["sync_status"]);
    expect(NOTEBOOK_DOC_PHASES).toEqual(["pending", "syncing", "interactive"]);
    expect(RUNTIME_STATE_PHASES).toEqual(["pending", "syncing", "ready"]);
    expect(INITIAL_LOAD_PHASES).toEqual(["not_needed", "streaming", "ready", "failed"]);
  });

  it("identifies display-capable Jupyter output types", () => {
    expect(DISPLAY_CAPABLE_JUPYTER_OUTPUT_TYPES).toEqual(["execute_result", "display_data"]);
    expect(isDisplayCapableJupyterOutputType("execute_result")).toBe(true);
    expect(isDisplayCapableJupyterOutputType("display_data")).toBe(true);
    expect(isDisplayCapableJupyterOutputType("stream")).toBe(false);
    expect(isDisplayCapableJupyterOutputType("error")).toBe(false);
    expect(isDisplayCapableJupyterOutputType(undefined)).toBe(false);
  });

  it("identifies display-capable Jupyter output objects", () => {
    expect(
      isDisplayCapableJupyterOutput({
        output_type: "display_data",
        data: { "text/plain": "ok" },
      }),
    ).toBe(true);
    expect(
      isDisplayCapableJupyterOutput({
        output_type: "execute_result",
        data: { "text/plain": "ok" },
        execution_count: 1,
      }),
    ).toBe(true);
    expect(isDisplayCapableJupyterOutput({ output_type: "stream" })).toBe(false);
    expect(isDisplayCapableJupyterOutput(null)).toBe(false);
  });

  it("selects initial-load failed and streaming states", () => {
    const failed: InitialLoadPhase = { phase: "failed", reason: "bad snapshot" };
    const streaming: InitialLoadPhase = { phase: "streaming" };
    const ready: InitialLoadPhase = { phase: "ready" };

    expect(isInitialLoadFailed(failed)).toBe(true);
    expect(isInitialLoadFailed(streaming)).toBe(false);
    expect(isInitialLoadStreaming(streaming)).toBe(true);
    expect(isInitialLoadStreaming(ready)).toBe(false);
  });
});
