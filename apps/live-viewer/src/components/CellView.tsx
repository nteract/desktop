/**
 * Full component tree integration for the live-viewer.
 *
 * Uses the same shared components as the notebook app:
 * - CellContainer (segmented ribbon, gutter, right-gutter layout)
 * - CompactExecutionButton (execution count + play/stop)
 * - OutputArea (iframe isolation, MediaRouter, ANSI, error rendering)
 * - CodeMirrorEditor (syntax highlighting, read-only)
 *
 * Coupling boundaries documented inline where the viewer diverges
 * from the notebook app's CodeCell:
 * - No useCrdtBridge (read-only, no editing)
 * - No useCellKeyboardNavigation (no cell focus navigation)
 * - No usePresenceContext (no remote cursors)
 * - No useEditorRegistry (no editor instance tracking)
 * - No cell-ui-state hooks (focus/executing/queued driven by props)
 * - No HistorySearchDialog (no Ctrl+R)
 * - No drag handles (no reordering)
 */

import { memo, useMemo } from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import { CompactExecutionButton } from "@/components/cell/CompactExecutionButton";
import { OutputArea } from "@/components/cell/OutputArea";
import { CodeMirrorEditor } from "@/components/editor/codemirror-editor";
import type { JupyterOutput } from "@/components/cell/jupyter-output";
import type { ExecutionState } from "runtimed/src/runtime-state";

interface CellData {
  id: string;
  cell_type: string;
  source: string;
  execution_count: number | null;
  outputs: JupyterOutput[];
  metadata?: Record<string, unknown>;
}

interface Props {
  cell: CellData;
  executionState: ExecutionState | null;
}

export const CellView = memo(function CellView({ cell, executionState }: Props) {
  const isRunning = executionState?.status === "running";
  const isQueued = executionState?.status === "queued";

  // Shared components from the real app's CodeCell — read-only mode
  // COUPLING EDGE: CodeCell uses useCrdtBridge for live editing.
  // We skip it entirely since this is read-only.
  const isCode = cell.cell_type === "code";
  const isMarkdown = cell.cell_type === "markdown";

  // Check source_hidden / outputs_hidden (JupyterLab convention)
  const jupyter = cell.metadata?.jupyter as
    | { source_hidden?: boolean; outputs_hidden?: boolean }
    | undefined;
  const isSourceHidden = jupyter?.source_hidden === true;
  const isOutputsHidden = jupyter?.outputs_hidden === true;

  // Render markdown cells as rendered output via OutputArea's iframe
  // isolation (same path as text/markdown MIME in execute_result).
  // COUPLING EDGE: The real MarkdownCell uses IsolatedFrame directly
  // with edit/preview toggle and CRDT bridge. We fake it as an output.
  const markdownAsOutput: JupyterOutput[] = useMemo(() => {
    if (!isMarkdown || !cell.source) return [];
    return [{
      output_type: "display_data" as const,
      data: { "text/markdown": cell.source },
      metadata: {},
    }];
  }, [isMarkdown, cell.source]);

  return (
    <CellContainer
      id={cell.id}
      cellType={cell.cell_type}
      gutterContent={
        isCode ? (
          <CompactExecutionButton
            count={cell.execution_count}
            isExecuting={isRunning}
            isQueued={isQueued}
          />
        ) : undefined
      }
      codeContent={
        isSourceHidden ? (
          <div className="flex items-center text-xs text-muted-foreground italic py-0.5">
            source hidden
          </div>
        ) : isMarkdown ? (
          <OutputArea
            outputs={markdownAsOutput}
            cellId={cell.id}
            preloadIframe
          />
        ) : (
          <CodeMirrorEditor
            initialValue={cell.source}
            language={isCode ? "python" : undefined}
            readOnly
          />
        )
      }
      outputContent={
        isOutputsHidden ? undefined : isCode && cell.outputs.length > 0 ? (
          <OutputArea
            outputs={cell.outputs}
            cellId={cell.id}
            preloadIframe
          />
        ) : undefined
      }
      hideOutput={isCode && cell.outputs.length === 0}
    />
  );
});
