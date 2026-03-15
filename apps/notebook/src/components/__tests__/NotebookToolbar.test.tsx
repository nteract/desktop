import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { KERNEL_STATUS } from "../../lib/kernel-status";
import { NotebookToolbar } from "../NotebookToolbar";

function renderToolbar(kernelStatus = KERNEL_STATUS.NOT_STARTED) {
  render(
    <NotebookToolbar
      kernelStatus={kernelStatus}
      kernelErrorMessage={null}
      envSource={null}
      envTypeHint={null}
      dirty={false}
      envProgress={null}
      runtime="python"
      onSave={vi.fn()}
      onStartKernel={vi.fn()}
      onInterruptKernel={vi.fn()}
      onRestartKernel={vi.fn()}
      onRunAllCells={vi.fn()}
      onRestartAndRunAll={vi.fn()}
      onAddCell={vi.fn()}
      onToggleDependencies={vi.fn()}
    />,
  );
}

describe("NotebookToolbar", () => {
  it("keeps the toolbar responsive while preventing button labels from wrapping", () => {
    renderToolbar();

    expect(
      screen.getByTestId("notebook-toolbar").firstElementChild,
    ).toHaveClass("flex-wrap");
    expect(screen.getByTestId("save-button")).toHaveClass("whitespace-nowrap");
    expect(screen.getByTestId("run-all-button")).toHaveClass(
      "whitespace-nowrap",
    );
  });

  it("exposes an accessible label for the restart and run all control", () => {
    renderToolbar(KERNEL_STATUS.IDLE);

    expect(
      screen.getByRole("button", {
        name: "Restart kernel and run all cells",
      }),
    ).toBeInTheDocument();
  });
});
