export const GITHUB_NEW_ISSUE_URL =
  "https://github.com/nteract/desktop/issues/new";
export const ISSUE_URL_MAX_LENGTH = 7000;

const OVERSIZE_BODY_NOTE =
  "Full diagnostics could not be added to the URL because the report is too large. Please paste the copied diagnostics report below.";

export interface PreparedIssueReport {
  diagnostics_markdown: string;
}

export interface IssueSubmissionRequest {
  title: string;
  description: string;
}

export interface IssueSubmissionDeps {
  prepareIssueReport: () => Promise<PreparedIssueReport>;
  openIssueUrl: (url: string) => void | Promise<void>;
  copyToClipboard: (text: string) => Promise<void>;
  maxUrlLength?: number;
}

export type IssueSubmissionResult =
  | { status: "opened" }
  | {
      status: "manual_copy_required";
      minimalIssueUrl: string;
      reportMarkdown: string;
      note: string;
    };

type IssuePrefillPlan =
  | { kind: "prefilled"; url: string; reportMarkdown: string }
  | { kind: "clipboard_fallback"; url: string; reportMarkdown: string };

function truncateMinimalDescription(description: string): string {
  const trimmed = description.trim();
  if (trimmed.length <= 600) {
    return trimmed;
  }
  return `${trimmed.slice(0, 600)}…`;
}

export function buildIssueBody(
  description: string,
  diagnosticsMarkdown: string,
): string {
  const safeDiagnostics =
    diagnosticsMarkdown.trim() || "_diagnostics unavailable_";
  return `## Summary\n\n${description.trim()}\n\n${safeDiagnostics}`;
}

export function buildIssueUrl(title: string, body: string): string {
  const url = new URL(GITHUB_NEW_ISSUE_URL);
  url.searchParams.set("title", title.trim());
  url.searchParams.set("body", body);
  return url.toString();
}

export function createIssuePrefillPlan(
  request: IssueSubmissionRequest,
  diagnosticsMarkdown: string,
  maxUrlLength = ISSUE_URL_MAX_LENGTH,
): IssuePrefillPlan {
  const reportMarkdown = buildIssueBody(
    request.description,
    diagnosticsMarkdown,
  );
  const prefilledUrl = buildIssueUrl(request.title, reportMarkdown);
  if (prefilledUrl.length <= maxUrlLength) {
    return { kind: "prefilled", url: prefilledUrl, reportMarkdown };
  }

  const minimalBody = `## Summary\n\n${truncateMinimalDescription(
    request.description,
  )}\n\n> ${OVERSIZE_BODY_NOTE}`;
  return {
    kind: "clipboard_fallback",
    url: buildIssueUrl(request.title, minimalBody),
    reportMarkdown,
  };
}

export async function submitIssueReport(
  request: IssueSubmissionRequest,
  deps: IssueSubmissionDeps,
): Promise<IssueSubmissionResult> {
  const diagnostics = await deps.prepareIssueReport();
  const plan = createIssuePrefillPlan(
    request,
    diagnostics.diagnostics_markdown,
    deps.maxUrlLength,
  );

  if (plan.kind === "prefilled") {
    await deps.openIssueUrl(plan.url);
    return { status: "opened" };
  }

  try {
    await deps.copyToClipboard(plan.reportMarkdown);
  } catch {
    return {
      status: "manual_copy_required",
      minimalIssueUrl: plan.url,
      reportMarkdown: plan.reportMarkdown,
      note: "Clipboard write failed. Copy the report below manually, then open the GitHub issue page.",
    };
  }

  await deps.openIssueUrl(plan.url);
  return { status: "opened" };
}
