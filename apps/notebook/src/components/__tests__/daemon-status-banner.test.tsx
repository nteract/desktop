/**
 * Tests for DaemonStatusBanner:
 * - State-driven rendering (hidden for ready/null, amber for failed, blue for progress)
 * - Progress message generation with attempt counter interpolation
 * - Conditional retry/dismiss button rendering
 * - Guidance text display
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vite-plus/test";
import { type DaemonStatus, DaemonStatusBanner } from "../DaemonStatusBanner";

describe("DaemonStatusBanner", () => {
  describe("visibility", () => {
    it("renders nothing when status is null", () => {
      const { container } = render(<DaemonStatusBanner status={null} />);
      expect(container.firstChild).toBeNull();
    });

    it("renders nothing when status is ready", () => {
      const { container } = render(
        <DaemonStatusBanner status={{ status: "ready", endpoint: "ws://localhost:8080" }} />,
      );
      expect(container.firstChild).toBeNull();
    });

    it("renders for checking status", () => {
      render(<DaemonStatusBanner status={{ status: "checking" }} />);
      expect(screen.getByText("Checking runtime status...")).toBeInTheDocument();
    });

    it("renders for failed status", () => {
      render(
        <DaemonStatusBanner
          status={{
            status: "failed",
            error: "Connection refused",
          }}
        />,
      );
      expect(screen.getByText("Runtime unavailable")).toBeInTheDocument();
      expect(screen.getByText("Connection refused")).toBeInTheDocument();
    });
  });

  describe("progress messages", () => {
    const cases: [DaemonStatus, string][] = [
      [{ status: "checking" }, "Checking runtime status..."],
      [{ status: "installing" }, "Installing runtime (first launch)..."],
      [{ status: "upgrading" }, "Upgrading runtime..."],
      [{ status: "starting" }, "Starting runtime..."],
      [{ status: "waiting_for_ready", attempt: 3, max_attempts: 10 }, "Starting runtime (3/10)..."],
    ];

    for (const [status, expectedMessage] of cases) {
      it(`shows "${expectedMessage}" for ${(status as { status: string }).status}`, () => {
        render(<DaemonStatusBanner status={status} />);
        expect(screen.getByText(expectedMessage)).toBeInTheDocument();
      });
    }

    it("interpolates different attempt/max_attempts values", () => {
      render(
        <DaemonStatusBanner
          status={{
            status: "waiting_for_ready",
            attempt: 7,
            max_attempts: 15,
          }}
        />,
      );
      expect(screen.getByText("Starting runtime (7/15)...")).toBeInTheDocument();
    });
  });

  describe("failed state", () => {
    it("shows retry button when onRetry is provided", () => {
      const onRetry = vi.fn();
      render(<DaemonStatusBanner status={{ status: "failed", error: "err" }} onRetry={onRetry} />);
      expect(screen.getByText("Retry")).toBeInTheDocument();
    });

    it("hides retry button when onRetry is not provided", () => {
      render(<DaemonStatusBanner status={{ status: "failed", error: "err" }} />);
      expect(screen.queryByText("Retry")).not.toBeInTheDocument();
    });

    it("calls onRetry when retry button is clicked", async () => {
      const onRetry = vi.fn();
      render(<DaemonStatusBanner status={{ status: "failed", error: "err" }} onRetry={onRetry} />);
      await userEvent.click(screen.getByText("Retry"));
      expect(onRetry).toHaveBeenCalledOnce();
    });

    it("shows dismiss button when onDismiss is provided", () => {
      render(
        <DaemonStatusBanner status={{ status: "failed", error: "err" }} onDismiss={vi.fn()} />,
      );
      expect(screen.getByLabelText("Dismiss")).toBeInTheDocument();
    });

    it("hides dismiss button when onDismiss is not provided", () => {
      render(<DaemonStatusBanner status={{ status: "failed", error: "err" }} />);
      expect(screen.queryByLabelText("Dismiss")).not.toBeInTheDocument();
    });

    it("shows guidance text when guidance is provided", () => {
      render(
        <DaemonStatusBanner
          status={{
            status: "failed",
            error: "err",
            guidance: "Try restarting the daemon",
          }}
        />,
      );
      expect(screen.getByText("Try restarting the daemon")).toBeInTheDocument();
    });

    it("hides guidance text when guidance is absent", () => {
      render(<DaemonStatusBanner status={{ status: "failed", error: "err" }} />);
      // Only the error should be present, no extra text
      expect(screen.queryByText("Try restarting")).not.toBeInTheDocument();
    });
  });
});
