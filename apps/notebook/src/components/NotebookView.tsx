import { Plus, RotateCcw, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef } from "react";
import { Button } from "@/components/ui/button";
import type { Runtime } from "@/hooks/useSyncedSettings";
import { ErrorBoundary } from "@/lib/error-boundary";
import type { CellPagePayload } from "../App";
import {
  EditorRegistryProvider,
  useEditorRegistry,
} from "../hooks/useEditorRegistry";
import type { FindMatch } from "../hooks/useGlobalFind";
import type { NotebookCell } from "../types";
import { CodeCell } from "./CodeCell";
import { MarkdownCell } from "./MarkdownCell";

interface NotebookViewProps {
  cells: NotebookCell[];
  focusedCellId: string | null;
  executingCellIds: Set<string>;
  pagePayloads: Map<string, CellPagePayload>;
  runtime?: Runtime;
  searchQuery?: string;
  searchCurrentMatch?: FindMatch | null;
  onFocusCell: (cellId: string) => void;
  onUpdateCellSource: (cellId: string, source: string) => void;
  onExecuteCell: (cellId: string) => void;
  onInterruptKernel: () => void;
  onDeleteCell: (cellId: string) => void;
  onAddCell: (type: "code" | "markdown", afterCellId?: string | null) => void;
  onClearPagePayload: (cellId: string) => void;
  onFormatCell?: (cellId: string) => void;
}

function AddCellButtons({
  afterCellId,
  onAdd,
}: {
  afterCellId?: string | null;
  onAdd: (type: "code" | "markdown", afterCellId?: string | null) => void;
}) {
  return (
    <div className="group/betweener flex h-4 w-full items-center">
      {/* Gutter spacer - matches cell gutter: action area + ribbon */}
      <div className="flex h-full flex-shrink-0">
        <div className="w-10" />
        <div className="w-1 bg-gray-200 dark:bg-gray-700" />
      </div>
      {/* Content area with centered buttons */}
      <div className="flex-1 relative flex items-center justify-center">
        {/* Thin line appears on hover */}
        <div className="absolute inset-x-0 h-px bg-transparent group-hover/betweener:bg-border transition-colors" />
        {/* Buttons appear on hover */}
        <div className="flex items-center gap-1 opacity-0 group-hover/betweener:opacity-100 transition-opacity z-10 bg-background px-2">
          <Button
            variant="ghost"
            size="sm"
            className="h-5 gap-1 px-2 text-xs text-muted-foreground hover:text-foreground"
            onClick={() => onAdd("code", afterCellId)}
          >
            <Plus className="h-3 w-3" />
            Code
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="h-5 gap-1 px-2 text-xs text-muted-foreground hover:text-foreground"
            onClick={() => onAdd("markdown", afterCellId)}
          >
            <Plus className="h-3 w-3" />
            Markdown
          </Button>
        </div>
      </div>
    </div>
  );
}

function CellErrorFallback({
  error,
  onRetry,
  onDelete,
}: {
  error: Error;
  onRetry: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="mx-4 my-2 rounded-md border border-destructive/50 bg-destructive/5 p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <p className="text-sm font-medium text-destructive">
            This cell encountered an error
          </p>
          <p className="mt-1 truncate text-xs text-muted-foreground">
            {error.message}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            variant="ghost"
            size="sm"
            onClick={onRetry}
            className="h-7 gap-1 px-2 text-xs"
            title="Retry rendering"
          >
            <RotateCcw className="h-3 w-3" />
            Retry
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={onDelete}
            className="h-7 gap-1 px-2 text-xs text-destructive hover:text-destructive"
            title="Delete cell"
          >
            <X className="h-3 w-3" />
            Delete
          </Button>
        </div>
      </div>
    </div>
  );
}

function NotebookViewContent({
  cells,
  focusedCellId,
  executingCellIds,
  pagePayloads,
  runtime = "python",
  searchQuery,
  searchCurrentMatch,
  onFocusCell,
  onUpdateCellSource,
  onExecuteCell,
  onInterruptKernel,
  onDeleteCell,
  onAddCell,
  onClearPagePayload,
  onFormatCell,
}: NotebookViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const { focusCell } = useEditorRegistry();

  // Memoize cell IDs array
  const cellIds = useMemo(() => cells.map((c) => c.id), [cells]);

  // Scroll the current search match cell into view
  useEffect(() => {
    if (!searchCurrentMatch) return;
    const cellEl = containerRef.current?.querySelector(
      `[data-cell-id="${searchCurrentMatch.cellId}"]`,
    );
    if (cellEl) {
      cellEl.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }, [searchCurrentMatch]);

  const renderCell = useCallback(
    (cell: NotebookCell, index: number) => {
      const isFocused = cell.id === focusedCellId;
      const isExecuting = executingCellIds.has(cell.id);

      // Navigation callbacks
      const onFocusPrevious = (cursorPosition: "start" | "end") => {
        if (index > 0) {
          const prevCellId = cellIds[index - 1];
          onFocusCell(prevCellId);
          focusCell(prevCellId, cursorPosition);
        }
      };

      const onFocusNext = (cursorPosition: "start" | "end") => {
        if (index < cellIds.length - 1) {
          const nextCellId = cellIds[index + 1];
          onFocusCell(nextCellId);
          focusCell(nextCellId, cursorPosition);
        }
      };

      if (cell.cell_type === "code") {
        const pagePayload = pagePayloads.get(cell.id) ?? null;
        // Use TypeScript for Deno, Python otherwise
        const language = runtime === "deno" ? "typescript" : "python";
        // Determine active match offset for this cell's source
        const activeSourceOffset =
          searchCurrentMatch &&
          searchCurrentMatch.cellId === cell.id &&
          searchCurrentMatch.type === "source"
            ? searchCurrentMatch.offset
            : -1;
        return (
          <CodeCell
            key={cell.id}
            cell={cell}
            language={language}
            isFocused={isFocused}
            isExecuting={isExecuting}
            pagePayload={pagePayload}
            searchQuery={searchQuery}
            searchActiveOffset={activeSourceOffset}
            onFocus={() => onFocusCell(cell.id)}
            onUpdateSource={(source) => onUpdateCellSource(cell.id, source)}
            onExecute={() => onExecuteCell(cell.id)}
            onInterrupt={onInterruptKernel}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onInsertCellAfter={() => onAddCell("code", cell.id)}
            onClearPagePayload={() => onClearPagePayload(cell.id)}
            onFormat={onFormatCell ? () => onFormatCell(cell.id) : undefined}
            isLastCell={index === cells.length - 1}
          />
        );
      }

      if (cell.cell_type === "markdown") {
        return (
          <MarkdownCell
            key={cell.id}
            cell={cell}
            isFocused={isFocused}
            searchQuery={searchQuery}
            onFocus={() => onFocusCell(cell.id)}
            onUpdateSource={(source) => onUpdateCellSource(cell.id, source)}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onInsertCellAfter={() => onAddCell("markdown", cell.id)}
            isLastCell={index === cells.length - 1}
          />
        );
      }

      // Raw cells rendered as plain text for now
      return (
        <div key={cell.id} className="px-4 py-2">
          <pre className="text-sm text-muted-foreground whitespace-pre-wrap">
            {cell.source}
          </pre>
        </div>
      );
    },
    [
      focusedCellId,
      executingCellIds,
      pagePayloads,
      runtime,
      searchQuery,
      searchCurrentMatch,
      cellIds,
      cells.length,
      onFocusCell,
      onUpdateCellSource,
      onExecuteCell,
      onInterruptKernel,
      onDeleteCell,
      onAddCell,
      onClearPagePayload,
      onFormatCell,
      focusCell,
    ],
  );

  return (
    <div
      ref={containerRef}
      className="flex-1 overflow-y-auto overflow-x-hidden py-4 pl-8 pr-4"
    >
      {cells.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-20 text-muted-foreground">
          <p className="text-sm">Empty notebook</p>
          <p className="text-xs mt-1">Add a cell to get started</p>
          <div className="mt-4 flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => onAddCell("code")}
              className="gap-1"
            >
              <Plus className="h-3 w-3" />
              Code Cell
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => onAddCell("markdown")}
              className="gap-1"
            >
              <Plus className="h-3 w-3" />
              Markdown Cell
            </Button>
          </div>
        </div>
      ) : (
        // biome-ignore lint/complexity/noUselessFragments: ternary else branch requires single expression
        <>
          {cells.map((cell, index) => (
            <div key={cell.id}>
              {index === 0 && (
                <AddCellButtons afterCellId={null} onAdd={onAddCell} />
              )}
              <ErrorBoundary
                fallback={(error, resetErrorBoundary) => (
                  <CellErrorFallback
                    error={error}
                    onRetry={resetErrorBoundary}
                    onDelete={() => onDeleteCell(cell.id)}
                  />
                )}
              >
                {renderCell(cell, index)}
              </ErrorBoundary>
              <AddCellButtons afterCellId={cell.id} onAdd={onAddCell} />
            </div>
          ))}
        </>
      )}
    </div>
  );
}

export function NotebookView(props: NotebookViewProps) {
  return (
    <EditorRegistryProvider>
      <NotebookViewContent {...props} />
    </EditorRegistryProvider>
  );
}
