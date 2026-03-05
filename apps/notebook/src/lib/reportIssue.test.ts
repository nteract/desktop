import { describe, expect, it, vi } from "vitest";
import { submitIssueReport } from "./reportIssue";

describe("submitIssueReport", () => {
  it("opens a prefilled GitHub issue URL on happy path", async () => {
    const openIssueUrl = vi.fn();
    const copyToClipboard = vi.fn();

    const result = await submitIssueReport(
      {
        title: "Kernel launch failed",
        description: "Kernel never starts after clicking run.",
      },
      {
        prepareIssueReport: async () => ({
          diagnostics_markdown: "## Diagnostics\n\n- os: `linux`",
        }),
        openIssueUrl,
        copyToClipboard,
      },
    );

    expect(result).toEqual({ status: "opened" });
    expect(openIssueUrl).toHaveBeenCalledTimes(1);
    expect(copyToClipboard).not.toHaveBeenCalled();

    const url = new URL(openIssueUrl.mock.calls[0][0] as string);
    expect(url.origin).toBe("https://github.com");
    expect(url.pathname).toBe("/nteract/desktop/issues/new");
    expect(url.searchParams.get("title")).toBe("Kernel launch failed");
    expect(url.searchParams.get("body")).toContain(
      "Kernel never starts after clicking run.",
    );
  });

  it("uses clipboard + minimal issue URL when prefill is oversized", async () => {
    const openIssueUrl = vi.fn();
    const copyToClipboard = vi.fn(async () => {});
    const diagnostics = "x".repeat(1000);

    const result = await submitIssueReport(
      {
        title: "Huge diagnostics report",
        description: "Repro details",
      },
      {
        prepareIssueReport: async () => ({
          diagnostics_markdown: diagnostics,
        }),
        openIssueUrl,
        copyToClipboard,
        maxUrlLength: 120,
      },
    );

    expect(result).toEqual({ status: "opened" });
    expect(copyToClipboard).toHaveBeenCalledTimes(1);
    expect(copyToClipboard.mock.calls[0][0]).toContain(diagnostics);
    expect(openIssueUrl).toHaveBeenCalledTimes(1);

    const url = new URL(openIssueUrl.mock.calls[0][0] as string);
    expect(url.searchParams.get("body")).toContain(
      "Full diagnostics could not be added to the URL",
    );
  });

  it("returns manual-copy fallback details when clipboard write fails", async () => {
    const openIssueUrl = vi.fn();
    const copyToClipboard = vi.fn(async () => {
      throw new Error("clipboard denied");
    });

    const result = await submitIssueReport(
      {
        title: "Clipboard fallback",
        description: "Need manual copy",
      },
      {
        prepareIssueReport: async () => ({
          diagnostics_markdown: "diagnostics block",
        }),
        openIssueUrl,
        copyToClipboard,
        maxUrlLength: 120,
      },
    );

    expect(result.status).toBe("manual_copy_required");
    if (result.status === "manual_copy_required") {
      expect(result.reportMarkdown).toContain("diagnostics block");
      expect(result.minimalIssueUrl).toContain("/nteract/desktop/issues/new");
      expect(result.note).toContain("Clipboard write failed");
    }
    expect(openIssueUrl).not.toHaveBeenCalled();
    expect(copyToClipboard).toHaveBeenCalledTimes(1);
  });
});
