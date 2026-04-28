import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vite-plus/test";
import { EnvBuildDecisionDialog, extractCondaEnvCreateCommand } from "../EnvBuildDecisionDialog";

const DETAILS =
  "environment.yml declares conda env 'analysis', which is not built on this machine. Run: conda env create -f /tmp/project/environment.yml";

describe("extractCondaEnvCreateCommand", () => {
  it("extracts only the terminal command from daemon details", () => {
    expect(extractCondaEnvCreateCommand(DETAILS)).toBe(
      "conda env create -f /tmp/project/environment.yml",
    );
  });

  it("returns null when no command is present", () => {
    expect(extractCondaEnvCreateCommand("environment missing")).toBeNull();
  });

  it("ignores text after the command line", () => {
    expect(extractCondaEnvCreateCommand(`${DETAILS}\nNext step: restart the kernel`)).toBe(
      "conda env create -f /tmp/project/environment.yml",
    );
  });
});

describe("EnvBuildDecisionDialog", () => {
  it("renders the environment build decision details", () => {
    render(
      <EnvBuildDecisionDialog
        open
        onOpenChange={() => {}}
        errorDetails={DETAILS}
        onRetry={() => {}}
      />,
    );

    expect(screen.getByTestId("env-build-decision-dialog")).toBeInTheDocument();
    expect(screen.getByText("Build environment.yml environment")).toBeInTheDocument();
    expect(screen.getByText(DETAILS)).toBeInTheDocument();
  });

  it("copies the extracted command", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    render(
      <EnvBuildDecisionDialog
        open
        onOpenChange={() => {}}
        errorDetails={DETAILS}
        onRetry={() => {}}
      />,
    );

    await user.click(screen.getByTestId("env-build-copy-button"));
    expect(writeText).toHaveBeenCalledWith("conda env create -f /tmp/project/environment.yml");
    expect(screen.getByText("Copied")).toBeInTheDocument();
  });

  it("invokes cancel and retry actions", async () => {
    const user = userEvent.setup();
    const onOpenChange = vi.fn();
    const onRetry = vi.fn();

    render(
      <EnvBuildDecisionDialog
        open
        onOpenChange={onOpenChange}
        errorDetails={DETAILS}
        onRetry={onRetry}
      />,
    );

    await user.click(screen.getByTestId("env-build-cancel-button"));
    expect(onOpenChange).toHaveBeenCalledWith(false);

    await user.click(screen.getByTestId("env-build-retry-button"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("disables copy when details do not include a command", () => {
    render(
      <EnvBuildDecisionDialog
        open
        onOpenChange={() => {}}
        errorDetails="environment.yml is missing a named env"
        onRetry={() => {}}
      />,
    );

    expect(screen.getByTestId("env-build-copy-button")).toBeDisabled();
  });
});
