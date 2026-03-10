import {
  DndContext,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS as DndCSS } from "@dnd-kit/utilities";
import { Plus, RotateCcw, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import type { Runtime } from "@/hooks/useSyncedSettings";
import { ErrorBoundary } from "@/lib/error-boundary";
import { cn } from "@/lib/utils";
import type { CellPagePayload } from "../App";
import {
  EditorRegistryProvider,
  useEditorRegistry,
} from "../hooks/useEditorRegistry";
import type { FindMatch } from "../hooks/useGlobalFind";
import type { NotebookCell } from "../types";
import { CellSkeleton } from "./CellSkeleton";
import { CodeCell } from "./CodeCell";
import { MarkdownCell } from "./MarkdownCell";

interface NotebookViewProps {
  cells: NotebookCell[];
  isLoading?: boolean;
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
  onMoveCell: (cellId: string, afterCellId?: string | null) => void;
  onAddCell: (type: "code" | "markdown", afterCellId?: string | null) => void;
  onClearPagePayload: (cellId: string) => void;
  onReportOutputMatchCount?: (cellId: string, count: number) => void;
}

function AddCellButtons({
  afterCellId,
  onAdd,
}: {
  afterCellId?: string | null;
  onAdd: (type: "code" | "markdown", afterCellId?: string | null) => void;
}) {
  return (
    <div className="group/betweener flex h-4 w-full items-center select-none">
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
        <div className="flex items-center gap-1 opacity-0 group-hover/betweener:opacity-100 transition-opacity z-10 bg-background px-2 select-none">
          <Button
            variant="ghost"
            size="sm"
            className="h-5 gap-1 px-2 text-xs text-muted-foreground hover:text-foreground select-none"
            onClick={() => onAdd("code", afterCellId)}
          >
            <Plus className="h-3 w-3" />
            Code
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="h-5 gap-1 px-2 text-xs text-muted-foreground hover:text-foreground select-none"
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

/**
 * Wrapper component that makes a cell sortable via drag-and-drop.
 * Uses the ribbon as a drag handle with visual feedback.
 */
function SortableCellWrapper({
  id,
  isDragging,
  isOver,
  children,
}: {
  id: string;
  isDragging: boolean;
  isOver: boolean;
  children: React.ReactNode;
}) {
  const { attributes, listeners, setNodeRef, transform, transition } =
    useSortable({ id });

  const style = {
    transform: DndCSS.Transform.toString(transform),
    transition,
  };

  return (
    <div
      ref={setNodeRef}
      style={style}
      className={cn(
        "relative",
        isDragging && "z-50 opacity-90",
        isOver && "sortable-cell-over",
      )}
    >
      {/* Drag handle overlay on the ribbon area */}
      <div
        {...attributes}
        {...listeners}
        role="button"
        tabIndex={0}
        className={cn(
          "absolute left-10 top-0 bottom-0 w-1 cursor-grab z-10",
          "hover:shadow-md hover:w-2 hover:-ml-0.5",
          "transition-all duration-150",
          isDragging && "cursor-grabbing shadow-lg w-2 -ml-0.5",
          // Show grip lines on hover
          "before:content-[''] before:absolute before:inset-x-0 before:top-1/2 before:-translate-y-1/2",
          "before:h-4 before:opacity-0 hover:before:opacity-40",
          "before:bg-[repeating-linear-gradient(0deg,currentColor,currentColor_1px,transparent_1px,transparent_3px)]",
          isDragging && "before:opacity-40",
        )}
        aria-label="Drag to reorder cell"
      />
      {/* Highlight indicator when another cell is dragging over this one */}
      {isOver && (
        <div className="absolute left-10 right-0 top-0 h-1 bg-sky-400 dark:bg-sky-500 z-20 rounded-full" />
      )}
      {children}
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
  isLoading = false,
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
  onMoveCell,
  onAddCell,
  onClearPagePayload,
  onReportOutputMatchCount,
}: NotebookViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const { focusCell } = useEditorRegistry();

  // Drag and drop state
  const [activeId, setActiveId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);

  // Sensor with slight activation delay to prevent accidental drags
  const sensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: {
        distance: 8, // 8px movement required to start drag
      },
    }),
  );

  const handleDragStart = useCallback((event: DragStartEvent) => {
    setActiveId(event.active.id as string);
  }, []);

  const handleDragOver = useCallback((event: DragOverEvent) => {
    setOverId(event.over?.id as string | null);
  }, []);

  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const { active, over } = event;
      setActiveId(null);
      setOverId(null);

      if (over && active.id !== over.id) {
        const oldIndex = cells.findIndex((c) => c.id === active.id);
        const newIndex = cells.findIndex((c) => c.id === over.id);
        if (oldIndex !== -1 && newIndex !== -1) {
          // Convert drop index to afterCellId for fractional indexing
          const afterCellId =
            newIndex === 0 ? null : (cells[newIndex - 1]?.id ?? null);
          onMoveCell(active.id as string, afterCellId);
        }
      }
    },
    [cells, onMoveCell],
  );

  const handleDragCancel = useCallback(() => {
    setActiveId(null);
    setOverId(null);
  }, []);

  // Memoize cell IDs array
  const cellIds = useMemo(() => cells.map((c) => c.id), [cells]);

  // Compute the cell ID that precedes the focused cell (keeps its output bright)
  const previousCellId = useMemo(() => {
    if (!focusedCellId) return null;
    const focusedIndex = cells.findIndex((c) => c.id === focusedCellId);
    return focusedIndex > 0 ? cells[focusedIndex - 1].id : null;
  }, [focusedCellId, cells]);

  // Prevent horizontal scroll drift (can happen during text selection)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const preventHorizontalScroll = () => {
      if (container.scrollLeft !== 0) {
        container.scrollLeft = 0;
      }
    };

    container.addEventListener("scroll", preventHorizontalScroll);
    return () =>
      container.removeEventListener("scroll", preventHorizontalScroll);
  }, []);

  // Scroll the current search match cell into view
  useEffect(() => {
    if (!searchCurrentMatch) return;
    const cellEl = containerRef.current?.querySelector(
      `[data-cell-id="${CSS.escape(searchCurrentMatch.cellId)}"]`,
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

      // Move cell callbacks
      const handleMoveUp =
        index > 0
          ? () => onMoveCell(cell.id, index >= 2 ? cells[index - 2]?.id : null)
          : undefined;
      const handleMoveDown =
        index < cellIds.length - 1
          ? () => onMoveCell(cell.id, cells[index + 1]?.id)
          : undefined;

      if (cell.cell_type === "code") {
        const pagePayload = pagePayloads.get(cell.id) ?? null;
        // Use TypeScript for Deno, IPython otherwise (for magic/shell highlighting)
        const language = runtime === "deno" ? "typescript" : "ipython";
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
            isPreviousCellFromFocused={cell.id === previousCellId}
            isExecuting={isExecuting}
            pagePayload={pagePayload}
            searchQuery={searchQuery}
            searchActiveOffset={activeSourceOffset}
            onSearchMatchCount={
              onReportOutputMatchCount
                ? (count: number) => onReportOutputMatchCount(cell.id, count)
                : undefined
            }
            onFocus={() => onFocusCell(cell.id)}
            onUpdateSource={(source) => onUpdateCellSource(cell.id, source)}
            onExecute={() => onExecuteCell(cell.id)}
            onInterrupt={onInterruptKernel}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onMoveUp={handleMoveUp}
            onMoveDown={handleMoveDown}
            onInsertCellAfter={() => onAddCell("code", cell.id)}
            onClearPagePayload={() => onClearPagePayload(cell.id)}
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
            isPreviousCellFromFocused={cell.id === previousCellId}
            searchQuery={searchQuery}
            onFocus={() => onFocusCell(cell.id)}
            onUpdateSource={(source) => onUpdateCellSource(cell.id, source)}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onMoveUp={handleMoveUp}
            onMoveDown={handleMoveDown}
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
      previousCellId,
      executingCellIds,
      pagePayloads,
      runtime,
      searchQuery,
      searchCurrentMatch,
      cellIds,
      cells,
      onFocusCell,
      onUpdateCellSource,
      onExecuteCell,
      onInterruptKernel,
      onDeleteCell,
      onMoveCell,
      onAddCell,
      onClearPagePayload,
      onReportOutputMatchCount,
      focusCell,
    ],
  );

  return (
    <div
      ref={containerRef}
      className="flex-1 overflow-y-auto overflow-x-clip overscroll-x-contain py-4 pl-8 pr-4"
      style={{ contain: "paint" }}
    >
      {cells.length === 0 ? (
        isLoading ? (
          <CellSkeleton />
        ) : (
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
        )
      ) : (
        <DndContext
          sensors={sensors}
          onDragStart={handleDragStart}
          onDragOver={handleDragOver}
          onDragEnd={handleDragEnd}
          onDragCancel={handleDragCancel}
        >
          <SortableContext
            items={cellIds}
            strategy={verticalListSortingStrategy}
          >
            {cells.map((cell, index) => (
              <SortableCellWrapper
                key={cell.id}
                id={cell.id}
                isDragging={activeId === cell.id}
                isOver={overId === cell.id && activeId !== cell.id}
              >
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
              </SortableCellWrapper>
            ))}
          </SortableContext>
        </DndContext>
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
