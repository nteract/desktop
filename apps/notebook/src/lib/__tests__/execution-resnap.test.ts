import { describe, expect, it } from "vite-plus/test";
import { decideExecutionResnap } from "../execution-resnap";

describe("decideExecutionResnap", () => {
  it("does not resnap after user cancellation during active execution", () => {
    expect(
      decideExecutionResnap({
        focusedCellId: "cell-1",
        outputsVersion: 2,
        executingCellCount: 1,
        resnapCancelled: true,
        resnapUntil: 0,
        now: 1000,
      }),
    ).toEqual({ shouldResnap: false, nextResnapUntil: 0 });
  });

  it("extends the active resnap window while execution is running", () => {
    expect(
      decideExecutionResnap({
        focusedCellId: "cell-1",
        outputsVersion: 2,
        executingCellCount: 1,
        resnapCancelled: false,
        resnapUntil: 0,
        now: 1000,
        activeWindowMs: 3500,
      }),
    ).toEqual({ shouldResnap: true, nextResnapUntil: 4500 });
  });

  it("keeps resnapping during the post-execution cooldown", () => {
    expect(
      decideExecutionResnap({
        focusedCellId: "cell-1",
        outputsVersion: 3,
        executingCellCount: 0,
        resnapCancelled: false,
        resnapUntil: 3000,
        now: 2000,
      }),
    ).toEqual({ shouldResnap: true, nextResnapUntil: 3000 });
  });

  it("stops after the post-execution cooldown expires", () => {
    expect(
      decideExecutionResnap({
        focusedCellId: "cell-1",
        outputsVersion: 3,
        executingCellCount: 0,
        resnapCancelled: false,
        resnapUntil: 3000,
        now: 3001,
      }),
    ).toEqual({ shouldResnap: false, nextResnapUntil: 3000 });
  });

  it("ignores initial output state and missing focus", () => {
    expect(
      decideExecutionResnap({
        focusedCellId: "cell-1",
        outputsVersion: 0,
        executingCellCount: 1,
        resnapCancelled: false,
        resnapUntil: 0,
        now: 1000,
      }).shouldResnap,
    ).toBe(false);

    expect(
      decideExecutionResnap({
        focusedCellId: null,
        outputsVersion: 1,
        executingCellCount: 1,
        resnapCancelled: false,
        resnapUntil: 0,
        now: 1000,
      }).shouldResnap,
    ).toBe(false);
  });
});
