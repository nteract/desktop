/**
 * Tests for KernelLaunchErrorBanner:
 * - Renders the stderr tail preserving newlines
 * - Retry button invokes onRetry callback
 * - Dismiss button invokes onDismiss callback
 * - Heading + icon rendered
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vite-plus/test";
import { KernelLaunchErrorBanner } from "../KernelLaunchErrorBanner";

const STDERR_TAIL = [
  "Kernel process exited immediately: exit status: 1",
  "stderr tail:",
  "/path/to/python: No module named nteract_kernel_launcher",
].join("\n");

describe("KernelLaunchErrorBanner", () => {
  it("shows the failure heading", () => {
    render(
      <KernelLaunchErrorBanner
        errorDetails={STDERR_TAIL}
        onRetry={() => {}}
        onDismiss={() => {}}
      />,
    );
    expect(screen.getByText("Kernel failed to start")).toBeInTheDocument();
  });

  it("renders the details string in a <pre> preserving the raw newlines", () => {
    render(
      <KernelLaunchErrorBanner
        errorDetails={STDERR_TAIL}
        onRetry={() => {}}
        onDismiss={() => {}}
      />,
    );
    // RTL's default matcher normalizes whitespace, so look at the
    // underlying <pre> node directly — it preserves the \n from the
    // daemon's stderr tail.
    const pre = screen.getByText((_, element) => element?.tagName.toLowerCase() === "pre");
    expect(pre.textContent).toBe(STDERR_TAIL);
  });

  it("invokes onRetry when Retry is clicked", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    render(
      <KernelLaunchErrorBanner errorDetails={STDERR_TAIL} onRetry={onRetry} onDismiss={() => {}} />,
    );
    await user.click(screen.getByRole("button", { name: /retry/i }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("invokes onDismiss when the X is clicked", async () => {
    const user = userEvent.setup();
    const onDismiss = vi.fn();
    render(
      <KernelLaunchErrorBanner
        errorDetails={STDERR_TAIL}
        onRetry={() => {}}
        onDismiss={onDismiss}
      />,
    );
    await user.click(screen.getByRole("button", { name: /dismiss/i }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
