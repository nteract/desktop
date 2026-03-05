import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ReportIssueDialog } from "./ReportIssueDialog";

describe("ReportIssueDialog", () => {
  it("validates that title and description are required", async () => {
    const onSubmit = vi.fn(async () => ({ status: "opened" as const }));

    render(
      <ReportIssueDialog
        open={true}
        onOpenChange={vi.fn()}
        onSubmit={onSubmit}
      />,
    );

    fireEvent.click(screen.getByTestId("report-issue-submit"));

    expect(
      await screen.findByTestId("report-issue-title-error"),
    ).toHaveTextContent("Title is required.");
    expect(
      await screen.findByTestId("report-issue-description-error"),
    ).toHaveTextContent("Description is required.");
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows manual-copy fallback when submit returns clipboard failure state", async () => {
    const onSubmit = vi.fn(async () => ({
      status: "manual_copy_required" as const,
      minimalIssueUrl: "https://github.com/nteract/desktop/issues/new",
      reportMarkdown: "## Summary\n\nmanual copy diagnostics",
      note: "Clipboard write failed. Copy manually.",
    }));

    render(
      <ReportIssueDialog
        open={true}
        onOpenChange={vi.fn()}
        onSubmit={onSubmit}
      />,
    );

    fireEvent.change(screen.getByLabelText("Title"), {
      target: { value: "Issue title" },
    });
    fireEvent.change(screen.getByLabelText("Description"), {
      target: { value: "Issue description" },
    });

    fireEvent.click(screen.getByTestId("report-issue-submit"));

    await waitFor(() => {
      expect(
        screen.getByTestId("report-issue-manual-copy"),
      ).toBeInTheDocument();
    });
    expect(screen.getByTestId("report-issue-manual-copy")).toHaveTextContent(
      "Clipboard write failed. Copy manually.",
    );
    expect(
      (
        screen.getByTestId(
          "report-issue-manual-copy-textarea",
        ) as HTMLTextAreaElement
      ).value,
    ).toContain("manual copy diagnostics");
  });
});
