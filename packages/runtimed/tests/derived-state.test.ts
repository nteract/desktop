import { describe, expect, it } from "vitest";
import { deriveQueueState } from "../src/derived-state";
import { DEFAULT_RUNTIME_STATE } from "../src/runtime-state";

describe("deriveQueueState", () => {
  it("projects queued executions before the kernel queue catches up", () => {
    const queue = deriveQueueState({
      ...DEFAULT_RUNTIME_STATE,
      executions: {
        "exec-2": {
          cell_id: "cell-2",
          status: "queued",
          execution_count: null,
          success: null,
          seq: 2,
        },
        "exec-1": {
          cell_id: "cell-1",
          status: "queued",
          execution_count: null,
          success: null,
          seq: 1,
        },
      },
    });

    expect(queue.queued).toEqual([
      { cell_id: "cell-1", execution_id: "exec-1" },
      { cell_id: "cell-2", execution_id: "exec-2" },
    ]);
  });

  it("does not duplicate runtime-agent queue entries", () => {
    const queue = deriveQueueState({
      ...DEFAULT_RUNTIME_STATE,
      queue: {
        executing: null,
        queued: [{ cell_id: "cell-1", execution_id: "exec-1" }],
      },
      executions: {
        "exec-1": {
          cell_id: "cell-1",
          status: "queued",
          execution_count: null,
          success: null,
          seq: 1,
        },
      },
    });

    expect(queue.queued).toEqual([{ cell_id: "cell-1", execution_id: "exec-1" }]);
  });
});
