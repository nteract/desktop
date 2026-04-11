import { useEffect, useRef, useState } from "react";
import type { CellData } from "../types";
import { getPreviewText } from "../lib/rich-output";
import { CodeBlock } from "./code-block";
import { ErrorOutput } from "./error-output";
import { MimeRenderer, StreamOutput } from "./mime-renderer";

interface CellProps {
  cell: CellData;
  blobBaseUrl?: string;
  defaultExpanded: boolean;
  forceExpanded?: boolean | null;
  /** Hide the source toggle (single-cell responses don't need it). */
  hideSource?: boolean;
}

const STATUS_ICONS: Record<string, string> = {
  done: "✓",
  error: "✗",
  cancelled: "⊘",
  running: "◐",
  queued: "⧗",
};

export function Cell({ cell, blobBaseUrl, defaultExpanded, forceExpanded, hideSource }: CellProps) {
  const [manualExpanded, setManualExpanded] = useState<boolean | null>(null);
  const prevForceExpanded = useRef(forceExpanded);

  // Reset manual toggle when expand-all/collapse-all changes,
  // so the new forceExpanded becomes the baseline and individual
  // clicks can override it again.
  useEffect(() => {
    if (forceExpanded !== prevForceExpanded.current) {
      setManualExpanded(null);
      prevForceExpanded.current = forceExpanded;
    }
  }, [forceExpanded]);

  // Priority: manual toggle > forceExpanded > default
  const expanded =
    manualExpanded != null
      ? manualExpanded
      : forceExpanded != null
        ? forceExpanded
        : defaultExpanded;

  const toggle = () => setManualExpanded(!expanded);

  const statusIcon = STATUS_ICONS[cell.status] || "";
  const statusClass =
    cell.status === "error"
      ? "status-error"
      : cell.status === "cancelled"
        ? "status-cancelled"
        : "status-done";
  const ecDisplay = cell.execution_count != null ? `[${cell.execution_count}]` : "";
  const preview = !expanded ? getPreviewText(cell) : "";

  return (
    <div className={`cell ${expanded ? "cell-expanded" : "cell-collapsed"}`}>
      <button type="button" className="cell-header" onClick={toggle}>
        <span className="cell-chevron">{expanded ? "▼" : "▶"}</span>
        {ecDisplay && <span className="cell-ec">{ecDisplay}</span>}
        {statusIcon && <span className={`cell-status ${statusClass}`}>{statusIcon}</span>}
        {!expanded && preview && <span className="cell-preview">{preview}</span>}
      </button>
      {expanded && (
        <div className="cell-body">
          {!hideSource && cell.source && (
            <details className="source-details">
              <summary className="source-summary">Source</summary>
              <CodeBlock code={cell.source} language="python" />
            </details>
          )}
          {cell.outputs?.length > 0 && (
            <div className="outputs">
              {cell.outputs.map((output, i) => {
                switch (output.output_type) {
                  case "stream":
                    return <StreamOutput key={i} output={output} />;
                  case "error":
                    return <ErrorOutput key={i} output={output} />;
                  case "display_data":
                  case "execute_result":
                    if (output.data) {
                      return <MimeRenderer key={i} data={output.data} blobBaseUrl={blobBaseUrl} />;
                    }
                    return null;
                  default:
                    return null;
                }
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
