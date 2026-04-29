import { describe, expect, it } from "vite-plus/test";

import {
  INITIAL_LOAD_PHASES,
  NOTEBOOK_DOC_PHASES,
  NOTEBOOK_REQUEST_TYPES,
  NOTEBOOK_RESPONSE_RESULTS,
  RUNTIME_STATE_PHASES,
  SESSION_CONTROL_TYPES,
} from "../src";

describe("protocol contract discriminants", () => {
  it("lists notebook request discriminants in wire order", () => {
    expect(NOTEBOOK_REQUEST_TYPES).toEqual([
      "launch_kernel",
      "execute_cell",
      "execute_cell_guarded",
      "clear_outputs",
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
      "outputs_cleared",
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
});
