import { open as openExternalUrl } from "@tauri-apps/plugin-shell";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type {
  IssueSubmissionRequest,
  IssueSubmissionResult,
} from "../lib/reportIssue";

interface ReportIssueDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSubmit: (request: IssueSubmissionRequest) => Promise<IssueSubmissionResult>;
}

export function ReportIssueDialog({
  open,
  onOpenChange,
  onSubmit,
}: ReportIssueDialogProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [titleError, setTitleError] = useState<string | null>(null);
  const [descriptionError, setDescriptionError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [manualCopyFallback, setManualCopyFallback] = useState<{
    note: string;
    reportMarkdown: string;
    minimalIssueUrl: string;
  } | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (open) {
      return;
    }
    setTitle("");
    setDescription("");
    setTitleError(null);
    setDescriptionError(null);
    setSubmitError(null);
    setManualCopyFallback(null);
    setSubmitting(false);
  }, [open]);

  const handleSubmit = async () => {
    const trimmedTitle = title.trim();
    const trimmedDescription = description.trim();
    const hasTitle = trimmedTitle.length > 0;
    const hasDescription = trimmedDescription.length > 0;
    setTitleError(hasTitle ? null : "Title is required.");
    setDescriptionError(hasDescription ? null : "Description is required.");
    if (!hasTitle || !hasDescription) {
      return;
    }

    setSubmitting(true);
    setSubmitError(null);
    setManualCopyFallback(null);
    try {
      const result = await onSubmit({
        title: trimmedTitle,
        description: trimmedDescription,
      });
      if (result.status === "opened") {
        onOpenChange(false);
        return;
      }
      setManualCopyFallback({
        note: result.note,
        reportMarkdown: result.reportMarkdown,
        minimalIssueUrl: result.minimalIssueUrl,
      });
    } catch (error) {
      setSubmitError(
        `Failed to prepare issue diagnostics: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl" data-testid="report-issue-dialog">
        <DialogHeader>
          <DialogTitle>Report an Issue</DialogTitle>
          <DialogDescription>
            Share what happened and include runtime diagnostics.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-foreground"
              htmlFor="issue-title"
            >
              Title
            </label>
            <Input
              id="issue-title"
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder="Brief issue summary"
              disabled={submitting}
            />
            {titleError && (
              <p
                className="text-xs text-destructive"
                data-testid="report-issue-title-error"
              >
                {titleError}
              </p>
            )}
          </div>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-foreground"
              htmlFor="issue-description"
            >
              Description
            </label>
            <Textarea
              id="issue-description"
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="What did you expect, and what happened instead?"
              className="min-h-36"
              disabled={submitting}
            />
            {descriptionError && (
              <p
                className="text-xs text-destructive"
                data-testid="report-issue-description-error"
              >
                {descriptionError}
              </p>
            )}
          </div>
          {submitError && (
            <p
              className="text-xs text-destructive"
              data-testid="report-issue-submit-error"
            >
              {submitError}
            </p>
          )}
          {manualCopyFallback && (
            <div
              className="space-y-3 rounded-md border border-amber-300 bg-amber-50/70 p-3 dark:border-amber-700 dark:bg-amber-900/20"
              data-testid="report-issue-manual-copy"
            >
              <p className="text-xs text-amber-800 dark:text-amber-300">
                {manualCopyFallback.note}
              </p>
              <Textarea
                readOnly
                value={manualCopyFallback.reportMarkdown}
                className="min-h-40 font-mono text-xs"
                data-testid="report-issue-manual-copy-textarea"
              />
              <Button
                variant="outline"
                onClick={() =>
                  openExternalUrl(manualCopyFallback.minimalIssueUrl)
                }
              >
                Open GitHub Issue Page
              </Button>
            </div>
          )}
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={submitting}
          >
            Cancel
          </Button>
          <Button
            onClick={handleSubmit}
            disabled={submitting}
            data-testid="report-issue-submit"
          >
            {submitting ? "Generating..." : "Submit"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
