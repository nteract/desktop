/**
 * Tests for NotebookToolbar logic:
 * - Kernel status cascade (which status text gets priority)
 * - Environment manager badge derivation (uv/conda/pixi from envSource)
 * - Start button visibility (hidden when kernel running)
 * - Interrupt button visibility (shown when kernel running, styled when busy)
 * - Kernel start selection logic (python3 preference, daemon mode)
 * - Deno install prompt (only on error with deno runtime)
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { EnvProgressState } from "../../hooks/useEnvProgress";
import { KERNEL_STATUS, type KernelStatus } from "../../lib/kernel-status";
import { NotebookToolbar } from "../NotebookToolbar";

function makeEnvProgress(
  overrides: Partial<EnvProgressState>,
): EnvProgressState {
  return {
    isActive: false,
    phase: null,
    envType: null,
    error: null,
    statusText: "",
    elapsedMs: null,
    progress: null,
    bytesPerSecond: null,
    currentPackage: null,
    ...overrides,
  };
}

const baseProps = {
  kernelStatus: KERNEL_STATUS.IDLE as KernelStatus,
  envSource: null as string | null,
  envProgress: null as EnvProgressState | null,
  onStartKernel: vi.fn(),
  onInterruptKernel: vi.fn(),
  onRestartKernel: vi.fn(),
  onRunAllCells: vi.fn(),
  onRestartAndRunAll: vi.fn(),
  onAddCell: vi.fn(),
  onToggleDependencies: vi.fn(),
};

describe("NotebookToolbar", () => {
  describe("start button visibility", () => {
    it("hides start button when kernel is idle", () => {
      render(
        <NotebookToolbar {...baseProps} kernelStatus={KERNEL_STATUS.IDLE} />,
      );
      expect(
        screen.queryByTestId("start-kernel-button"),
      ).not.toBeInTheDocument();
    });

    it("hides start button when kernel is busy", () => {
      render(
        <NotebookToolbar {...baseProps} kernelStatus={KERNEL_STATUS.BUSY} />,
      );
      expect(
        screen.queryByTestId("start-kernel-button"),
      ).not.toBeInTheDocument();
    });

    it("hides start button when kernel is starting", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.STARTING}
        />,
      );
      expect(
        screen.queryByTestId("start-kernel-button"),
      ).not.toBeInTheDocument();
    });

    it("shows start button when kernel is not started", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.NOT_STARTED}
        />,
      );
      expect(screen.getByTestId("start-kernel-button")).toBeInTheDocument();
    });

    it("shows start button when kernel is shut down", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.SHUTDOWN}
        />,
      );
      expect(screen.getByTestId("start-kernel-button")).toBeInTheDocument();
    });

    it("shows start button when kernel has errored", () => {
      render(
        <NotebookToolbar {...baseProps} kernelStatus={KERNEL_STATUS.ERROR} />,
      );
      expect(screen.getByTestId("start-kernel-button")).toBeInTheDocument();
    });
  });

  describe("interrupt button visibility", () => {
    it("shows interrupt button when kernel is running", () => {
      render(
        <NotebookToolbar {...baseProps} kernelStatus={KERNEL_STATUS.IDLE} />,
      );
      expect(screen.getByTestId("interrupt-kernel-button")).toBeInTheDocument();
    });

    it("hides interrupt button when kernel is not running", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.NOT_STARTED}
        />,
      );
      expect(
        screen.queryByTestId("interrupt-kernel-button"),
      ).not.toBeInTheDocument();
    });
  });

  describe("kernel start selection", () => {
    it("calls onStartKernel with empty string in daemon mode (no listKernelspecs)", async () => {
      const onStartKernel = vi.fn();
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.NOT_STARTED}
          onStartKernel={onStartKernel}
        />,
      );
      await userEvent.click(screen.getByTestId("start-kernel-button"));
      expect(onStartKernel).toHaveBeenCalledWith("");
    });

    it("prefers python3 from kernelspecs list", async () => {
      const onStartKernel = vi.fn();
      const listKernelspecs = vi.fn().mockResolvedValue([
        { name: "ir", display_name: "R", language: "r" },
        { name: "python3", display_name: "Python 3", language: "python" },
      ]);
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.NOT_STARTED}
          onStartKernel={onStartKernel}
          listKernelspecs={listKernelspecs}
        />,
      );
      // Wait for kernelspecs to load
      await vi.waitFor(() => {
        expect(listKernelspecs).toHaveBeenCalled();
      });
      await userEvent.click(screen.getByTestId("start-kernel-button"));
      expect(onStartKernel).toHaveBeenCalledWith("python3");
    });

    it("falls back to first available kernelspec when no python", async () => {
      const onStartKernel = vi.fn();
      const listKernelspecs = vi.fn().mockResolvedValue([
        { name: "ir", display_name: "R", language: "r" },
        { name: "julia", display_name: "Julia", language: "julia" },
      ]);
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.NOT_STARTED}
          onStartKernel={onStartKernel}
          listKernelspecs={listKernelspecs}
        />,
      );
      await vi.waitFor(() => {
        expect(listKernelspecs).toHaveBeenCalled();
      });
      await userEvent.click(screen.getByTestId("start-kernel-button"));
      expect(onStartKernel).toHaveBeenCalledWith("ir");
    });
  });

  describe("environment manager badge", () => {
    it("shows uv badge for python runtime with non-conda envSource", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="python"
          envSource="uv:/some/path"
          kernelStatus={KERNEL_STATUS.IDLE}
        />,
      );
      const toggle = screen.getByTestId("deps-toggle");
      expect(toggle.dataset.envManager).toBe("uv");
    });

    it("shows conda badge for conda envSource", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="python"
          envSource="conda:/some/env"
          kernelStatus={KERNEL_STATUS.IDLE}
        />,
      );
      const toggle = screen.getByTestId("deps-toggle");
      expect(toggle.dataset.envManager).toBe("conda");
    });

    it("shows pixi badge for pixi:toml envSource", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="python"
          envSource="pixi:toml"
          kernelStatus={KERNEL_STATUS.IDLE}
        />,
      );
      const toggle = screen.getByTestId("deps-toggle");
      expect(toggle.dataset.envManager).toBe("pixi");
    });

    it("uses envTypeHint when kernel is not idle/busy (e.g. during startup)", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="python"
          envSource={null}
          envTypeHint="conda"
          kernelStatus={KERNEL_STATUS.STARTING}
        />,
      );
      const toggle = screen.getByTestId("deps-toggle");
      expect(toggle.dataset.envManager).toBe("conda");
    });

    it("shows no env badge for deno runtime", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="deno"
          envSource="deno:/something"
          kernelStatus={KERNEL_STATUS.IDLE}
        />,
      );
      const toggle = screen.getByTestId("deps-toggle");
      expect(toggle.dataset.envManager).toBeUndefined();
    });

    it("hides runtime badge when runtime is null", () => {
      render(<NotebookToolbar {...baseProps} runtime={null} />);
      expect(screen.queryByTestId("deps-toggle")).not.toBeInTheDocument();
    });
  });

  describe("kernel status display", () => {
    it("shows kernel status text", () => {
      render(
        <NotebookToolbar {...baseProps} kernelStatus={KERNEL_STATUS.IDLE} />,
      );
      const status = screen.getByTestId("kernel-status");
      expect(status.dataset.kernelStatus).toBe("idle");
    });

    it("shows env progress status text when active", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.STARTING}
          envProgress={makeEnvProgress({
            isActive: true,
            statusText: "Installing packages...",
          })}
        />,
      );
      expect(screen.getByText("Installing packages...")).toBeInTheDocument();
    });

    it("shows env error status when env has error", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          kernelStatus={KERNEL_STATUS.STARTING}
          envProgress={makeEnvProgress({
            isActive: false,
            statusText: "Environment error",
            error: "pip install failed",
          })}
        />,
      );
      expect(screen.getByText("Environment error")).toBeInTheDocument();
    });
  });

  describe("deno install prompt", () => {
    it("shows deno install prompt when runtime=deno, status=error, and error message exists", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="deno"
          kernelStatus={KERNEL_STATUS.ERROR}
          kernelErrorMessage="Deno not found"
        />,
      );
      expect(screen.getByText(/Deno not available/)).toBeInTheDocument();
    });

    it("does not show deno prompt when runtime is python", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="python"
          kernelStatus={KERNEL_STATUS.ERROR}
          kernelErrorMessage="some error"
        />,
      );
      expect(screen.queryByText(/Deno not available/)).not.toBeInTheDocument();
    });

    it("does not show deno prompt when kernel is not in error", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="deno"
          kernelStatus={KERNEL_STATUS.IDLE}
          kernelErrorMessage="stale error"
        />,
      );
      expect(screen.queryByText(/Deno not available/)).not.toBeInTheDocument();
    });

    it("does not show deno prompt when no error message", () => {
      render(
        <NotebookToolbar
          {...baseProps}
          runtime="deno"
          kernelStatus={KERNEL_STATUS.ERROR}
          kernelErrorMessage={null}
        />,
      );
      expect(screen.queryByText(/Deno not available/)).not.toBeInTheDocument();
    });
  });
});
