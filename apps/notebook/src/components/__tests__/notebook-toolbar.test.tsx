import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { KERNEL_STATUS } from "../../lib/kernel-status";
import { NotebookToolbar } from "../NotebookToolbar";

describe("NotebookToolbar", () => {
  it("keeps run controls on one line and uses higher-contrast icon styling", () => {
    render(
      <NotebookToolbar
        kernelStatus={KERNEL_STATUS.IDLE}
        kernelErrorMessage={null}
        envSource={null}
        dirty={false}
        envProgress={null}
        onSave={() => {}}
        onStartKernel={() => {}}
        onInterruptKernel={() => {}}
        onRestartKernel={() => {}}
        onRunAllCells={() => {}}
        onRestartAndRunAll={() => {}}
        onAddCell={() => {}}
        onToggleDependencies={() => {}}
      />,
    );

    expect(
      screen
        .getByTestId("run-all-button")
        .className.includes("whitespace-nowrap"),
    ).toBe(true);
    expect(
      screen
        .getByTestId("run-all-button")
        .className.includes("[&_svg]:text-slate-600"),
    ).toBe(true);
    expect(
      screen
        .getByTestId("restart-kernel-button")
        .className.includes("whitespace-nowrap"),
    ).toBe(true);
  });
});
