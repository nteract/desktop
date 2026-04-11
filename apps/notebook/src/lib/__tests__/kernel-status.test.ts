import { describe, expect, it } from "vite-plus/test";
import {
  getKernelStatusLabel,
  isKernelStatus,
  KERNEL_STATUS,
  KERNEL_STATUS_LABELS,
  type KernelStatus,
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

describe("getKernelStatusLabel", () => {
  it.each(
    Object.entries(KERNEL_STATUS_LABELS) as [KernelStatus, string][],
  )("returns '%s' label as '%s'", (status, expectedLabel) => {
    expect(getKernelStatusLabel(status)).toBe(expectedLabel);
  });

  it("returns human-readable label for not_started", () => {
    expect(getKernelStatusLabel(KERNEL_STATUS.NOT_STARTED)).toBe(
      "initializing",
    );
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

describe("KERNEL_STATUS_LABELS", () => {
  it("has a label for every status", () => {
    for (const status of Object.values(KERNEL_STATUS)) {
      expect(KERNEL_STATUS_LABELS[status]).toBeDefined();
      expect(typeof KERNEL_STATUS_LABELS[status]).toBe("string");
    }
  });
});
