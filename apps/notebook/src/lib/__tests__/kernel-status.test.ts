import { describe, expect, it } from "vite-plus/test";
import {
  getLifecycleLabel,
  getStatusKeyLabel,
  isKernelStatus,
  KERNEL_STATUS,
  RUNTIME_STATUS,
  RUNTIME_STATUS_LABELS,
  type RuntimeLifecycle,
  type RuntimeStatusKey,
} from "../kernel-status";

describe("isKernelStatus", () => {
  it.each(
    Object.values(KERNEL_STATUS),
  )("returns true for valid status '%s'", (status) => {
    expect(isKernelStatus(status)).toBe(true);
  });

  it("returns false for unknown strings", () => {
    expect(isKernelStatus("running")).toBe(false);
    expect(isKernelStatus("stopped")).toBe(false);
    expect(isKernelStatus("")).toBe(false);
    expect(isKernelStatus("IDLE")).toBe(false); // case-sensitive
    expect(isKernelStatus("Busy")).toBe(false);
  });
});

describe("KERNEL_STATUS", () => {
  it("contains exactly seven statuses", () => {
    expect(Object.keys(KERNEL_STATUS)).toHaveLength(7);
  });

  it("has expected values", () => {
    expect(KERNEL_STATUS.NOT_STARTED).toBe("not_started");
    expect(KERNEL_STATUS.STARTING).toBe("starting");
    expect(KERNEL_STATUS.IDLE).toBe("idle");
    expect(KERNEL_STATUS.BUSY).toBe("busy");
    expect(KERNEL_STATUS.ERROR).toBe("error");
    expect(KERNEL_STATUS.SHUTDOWN).toBe("shutdown");
    expect(KERNEL_STATUS.AWAITING_TRUST).toBe("awaiting_trust");
  });
});

describe("getLifecycleLabel", () => {
  it("labels each lifecycle variant with its expanded text", () => {
    const cases: [RuntimeLifecycle, string | null, string][] = [
      [{ lifecycle: "NotStarted" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.NOT_STARTED]],
      [{ lifecycle: "AwaitingTrust" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.AWAITING_TRUST]],
      [{ lifecycle: "Resolving" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RESOLVING]],
      [{ lifecycle: "PreparingEnv" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.PREPARING_ENV]],
      [{ lifecycle: "Launching" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.LAUNCHING]],
      [{ lifecycle: "Connecting" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.CONNECTING]],
      [
        { lifecycle: "Running", activity: "Idle" },
        null,
        RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RUNNING_IDLE],
      ],
      [
        { lifecycle: "Running", activity: "Busy" },
        null,
        RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RUNNING_BUSY],
      ],
      [
        { lifecycle: "Running", activity: "Unknown" },
        null,
        RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RUNNING_UNKNOWN],
      ],
      [{ lifecycle: "Error" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.ERROR]],
      [{ lifecycle: "Shutdown" }, null, RUNTIME_STATUS_LABELS[RUNTIME_STATUS.SHUTDOWN]],
    ];
    for (const [lc, reason, expected] of cases) {
      expect(getLifecycleLabel(lc, reason)).toBe(expected);
    }
  });

  it("appends typed reason when lifecycle is Error", () => {
    expect(getLifecycleLabel({ lifecycle: "Error" }, "missing_ipykernel")).toBe(
      "error: missing_ipykernel",
    );
  });

  it("ignores reason for non-Error lifecycles", () => {
    expect(
      getLifecycleLabel({ lifecycle: "Running", activity: "Idle" }, "missing_ipykernel"),
    ).toBe(RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RUNNING_IDLE]);
  });

  it("treats empty-string reason as no reason", () => {
    expect(getLifecycleLabel({ lifecycle: "Error" }, "")).toBe(
      RUNTIME_STATUS_LABELS[RUNTIME_STATUS.ERROR],
    );
  });
});

describe("getStatusKeyLabel", () => {
  it("labels every runtime status key", () => {
    const keys = Object.values(RUNTIME_STATUS) as RuntimeStatusKey[];
    for (const key of keys) {
      expect(getStatusKeyLabel(key, null)).toBe(RUNTIME_STATUS_LABELS[key]);
    }
  });

  it("appends typed reason when key is error", () => {
    expect(getStatusKeyLabel(RUNTIME_STATUS.ERROR, "missing_ipykernel")).toBe(
      "error: missing_ipykernel",
    );
  });

  it("ignores reason for non-error keys", () => {
    expect(getStatusKeyLabel(RUNTIME_STATUS.RUNNING_IDLE, "missing_ipykernel")).toBe(
      RUNTIME_STATUS_LABELS[RUNTIME_STATUS.RUNNING_IDLE],
    );
  });

  it("treats empty-string reason as no reason", () => {
    expect(getStatusKeyLabel(RUNTIME_STATUS.ERROR, "")).toBe(
      RUNTIME_STATUS_LABELS[RUNTIME_STATUS.ERROR],
    );
  });
});
