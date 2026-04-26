/**
 * Tests for KernelLaunchErrorBanner:
 * - Renders the stderr tail preserving newlines
 * - Retry button invokes onRetry callback
 * - Dismiss button invokes onDismiss callback
 * - Heading + icon rendered
 * - Gating helper `shouldShowKernelLaunchErrorBanner` exhaustively
 *   covers the cases App.tsx composes against (typed reasons, runtime,
 *   lifecycle, details presence).
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { KERNEL_ERROR_REASON, type RuntimeLifecycle } from "runtimed";
import { describe, expect, it, vi } from "vite-plus/test";
import {
  KernelLaunchErrorBanner,
  shouldShowKernelLaunchErrorBanner,
} from "../KernelLaunchErrorBanner";

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

describe("shouldShowKernelLaunchErrorBanner", () => {
  const ERROR: RuntimeLifecycle = { lifecycle: "Error" };
  const IDLE: RuntimeLifecycle = { lifecycle: "Running", activity: "Idle" };

  it("shows for a plain Error with details and no typed reason", () => {
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails: STDERR_TAIL,
        errorReason: "",
        runtime: "python",
      }),
    ).toBe(true);
  });

  it("hides when lifecycle is not Error", () => {
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: IDLE,
        errorDetails: STDERR_TAIL,
        errorReason: null,
        runtime: "python",
      }),
    ).toBe(false);
  });

  it("hides when errorDetails is null or empty", () => {
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails: null,
        errorReason: null,
        runtime: "python",
      }),
    ).toBe(false);
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails: "",
        errorReason: null,
        runtime: "python",
      }),
    ).toBe(false);
  });

  it("hides for MissingIpykernel (toolbar prompt owns that UX)", () => {
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails: "ipykernel not declared",
        errorReason: KERNEL_ERROR_REASON.MISSING_IPYKERNEL,
        runtime: "python",
      }),
    ).toBe(false);
  });

  it("shows for CondaEnvYmlMissing — no dedicated UI exists yet", () => {
    // Codex review on #2236: excluding this reason suppressed the only
    // real rendered surface the daemon's `error_details` has.
    // `environment.yml` unbuilt-env failures now use the generic banner.
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails:
          "environment.yml declares conda env 'analysis', which is not built. Run: conda env create -f environment.yml",
        errorReason: KERNEL_ERROR_REASON.CONDA_ENV_YML_MISSING,
        runtime: "python",
      }),
    ).toBe(true);
  });

  it("hides for Deno runtime (toolbar renders its own install prompt)", () => {
    // Deno failures show `Deno not available. Auto-install failed…`
    // in NotebookToolbar. Rendering the red banner on top would be
    // duplicate noise with two retry surfaces.
    expect(
      shouldShowKernelLaunchErrorBanner({
        lifecycle: ERROR,
        errorDetails: "deno binary not on PATH",
        errorReason: null,
        runtime: "deno",
      }),
    ).toBe(false);
  });
});
