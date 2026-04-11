/**
 * Tests for TrustDialog component logic:
 * - Typosquat warning lookup with case normalization and version specifier parsing
 * - Dialog title/description variants based on trust status and daemon mode
 * - Async approval flow (close only on success)
 * - Loading state behavior
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vite-plus/test";
import type { TrustInfo, TyposquatWarning } from "../../hooks/useTrust";
import { TrustDialog } from "../TrustDialog";

function makeTrustInfo(overrides: Partial<TrustInfo> = {}): TrustInfo {
  return {
    status: "untrusted",
    uv_dependencies: [],
    conda_dependencies: [],
    conda_channels: [],
    ...overrides,
  };
}

const defaultProps = {
  open: true,
  onOpenChange: vi.fn(),
  trustInfo: makeTrustInfo({ uv_dependencies: ["requests"] }),
  typosquatWarnings: [] as TyposquatWarning[],
  onApprove: vi.fn().mockResolvedValue(true),
  onDecline: vi.fn(),
};

describe("TrustDialog", () => {
  describe("typosquat warning lookup", () => {
    it("matches package names case-insensitively", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["Requests"],
          })}
          typosquatWarnings={[{ package: "requests", similar_to: "requests2", distance: 1 }]}
        />,
      );
      expect(screen.getByText(/Similar to "requests2"/)).toBeInTheDocument();
    });

    it("strips version specifiers before looking up warnings", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["reqeusts>=2.0"],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.getByText(/Similar to "requests"/)).toBeInTheDocument();
    });

    it("strips bracket extras before lookup (e.g. package[extra]>=1.0)", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["reqeusts[security]>=2.0"],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.getByText(/Similar to "requests"/)).toBeInTheDocument();
    });

    it("strips @ version pinning (e.g. package@1.0)", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["reqeusts@2.28.0"],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.getByText(/Similar to "requests"/)).toBeInTheDocument();
    });

    it("strips semicolon environment markers", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ['reqeusts; python_version>="3.8"'],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.getByText(/Similar to "requests"/)).toBeInTheDocument();
    });

    it("shows no warning badge for packages not in the warning list", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["numpy", "pandas"],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.queryByText(/Similar to/)).not.toBeInTheDocument();
    });

    it("shows global typosquat alert banner when warnings exist", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["reqeusts"],
          })}
          typosquatWarnings={[{ package: "reqeusts", similar_to: "requests", distance: 1 }]}
        />,
      );
      expect(screen.getByText("Potential typosquatting detected")).toBeInTheDocument();
    });

    it("hides typosquat alert banner when no warnings", () => {
      render(<TrustDialog {...defaultProps} typosquatWarnings={[]} />);
      expect(screen.queryByText("Potential typosquatting detected")).not.toBeInTheDocument();
    });
  });

  describe("dialog title and description variants", () => {
    it("shows 'Dependencies Modified' title for signature_invalid status", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            status: "signature_invalid",
            uv_dependencies: ["numpy"],
          })}
        />,
      );
      expect(screen.getByText("Dependencies Modified")).toBeInTheDocument();
    });

    it("shows 'Review Dependencies' title for untrusted status", () => {
      render(<TrustDialog {...defaultProps} />);
      expect(screen.getByText("Review Dependencies")).toBeInTheDocument();
    });

    it("shows daemon-mode description mentioning auto-launch", () => {
      render(<TrustDialog {...defaultProps} daemonMode />);
      expect(screen.getByText(/the kernel will start automatically/)).toBeInTheDocument();
    });

    it("shows default description for non-daemon mode", () => {
      render(<TrustDialog {...defaultProps} daemonMode={false} />);
      expect(screen.getByText(/Review them before running code/)).toBeInTheDocument();
    });

    it("shows signature_invalid description when status is signature_invalid", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            status: "signature_invalid",
            uv_dependencies: ["numpy"],
          })}
        />,
      );
      expect(screen.getByText(/dependencies have been modified/)).toBeInTheDocument();
    });
  });

  describe("button labels", () => {
    it("shows 'Trust & Start' in daemon mode", () => {
      render(<TrustDialog {...defaultProps} daemonMode />);
      expect(screen.getByTestId("trust-approve-button")).toHaveTextContent("Trust & Start");
    });

    it("shows 'Trust & Install' in non-daemon mode", () => {
      render(<TrustDialog {...defaultProps} daemonMode={false} />);
      expect(screen.getByTestId("trust-approve-button")).toHaveTextContent("Trust & Install");
    });

    it("shows 'Approving...' when loading", () => {
      render(<TrustDialog {...defaultProps} loading />);
      expect(screen.getByTestId("trust-approve-button")).toHaveTextContent("Approving...");
    });

    it("disables both buttons when loading", () => {
      render(<TrustDialog {...defaultProps} loading />);
      expect(screen.getByTestId("trust-approve-button")).toBeDisabled();
      expect(screen.getByTestId("trust-decline-button")).toBeDisabled();
    });
  });

  describe("async approval flow", () => {
    it("closes dialog when onApprove resolves with true", async () => {
      const onOpenChange = vi.fn();
      const onApprove = vi.fn().mockResolvedValue(true);
      render(<TrustDialog {...defaultProps} onOpenChange={onOpenChange} onApprove={onApprove} />);

      await userEvent.click(screen.getByTestId("trust-approve-button"));
      await waitFor(() => {
        expect(onOpenChange).toHaveBeenCalledWith(false);
      });
    });

    it("does NOT close dialog when onApprove resolves with false", async () => {
      const onOpenChange = vi.fn();
      const onApprove = vi.fn().mockResolvedValue(false);
      render(<TrustDialog {...defaultProps} onOpenChange={onOpenChange} onApprove={onApprove} />);

      await userEvent.click(screen.getByTestId("trust-approve-button"));
      await waitFor(() => {
        expect(onApprove).toHaveBeenCalled();
      });
      // onOpenChange should NOT have been called with false
      expect(onOpenChange).not.toHaveBeenCalledWith(false);
    });

    it("calls onDecline and closes on decline button click", async () => {
      const onDecline = vi.fn();
      const onOpenChange = vi.fn();
      render(<TrustDialog {...defaultProps} onDecline={onDecline} onOpenChange={onOpenChange} />);

      await userEvent.click(screen.getByTestId("trust-decline-button"));
      expect(onDecline).toHaveBeenCalled();
      expect(onOpenChange).toHaveBeenCalledWith(false);
    });
  });

  describe("package list rendering", () => {
    it("shows UV dependencies under PyPI Packages heading", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: ["numpy", "pandas"],
          })}
        />,
      );
      expect(screen.getByText("PyPI Packages")).toBeInTheDocument();
      expect(screen.getByText("numpy")).toBeInTheDocument();
      expect(screen.getByText("pandas")).toBeInTheDocument();
    });

    it("shows Conda dependencies with channels", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            conda_dependencies: ["scipy", "matplotlib"],
            conda_channels: ["conda-forge", "defaults"],
          })}
        />,
      );
      expect(screen.getByText("Conda Packages")).toBeInTheDocument();
      expect(screen.getByText("scipy")).toBeInTheDocument();
      expect(screen.getByText("(conda-forge, defaults)")).toBeInTheDocument();
    });

    it("hides PyPI section when no UV dependencies", () => {
      render(
        <TrustDialog
          {...defaultProps}
          trustInfo={makeTrustInfo({
            uv_dependencies: [],
            conda_dependencies: ["scipy"],
          })}
        />,
      );
      expect(screen.queryByText("PyPI Packages")).not.toBeInTheDocument();
    });
  });
});
