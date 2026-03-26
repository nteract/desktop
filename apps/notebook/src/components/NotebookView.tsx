import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  DragOverlay,
  type DragStartEvent,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS as DndCSS } from "@dnd-kit/utilities";
import { Plus, RotateCcw, X } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { logger } from "../lib/logger";
import {
  getNotebookCellsSnapshot,
  useCell,
  useMaterializeVersion,
} from "../lib/notebook-cells";
import type { CodeCell as CodeCellType, NotebookCell } from "../types";
import { CellSkeleton } from "./CellSkeleton";
import { CodeCell } from "./CodeCell";
import { MarkdownCell } from "./MarkdownCell";
import { RawCell } from "./RawCell";

interface NotebookViewProps {
  cellIds: string[];
  isLoading?: boolean;
  focusedCellId: string | null;
  executingCellIds: Set<string>;
  queuedCellIds: Set<string>;
  pagePayloads: Map<string, CellPagePayload>;
  runtime?: Runtime | null;
  searchQuery?: string;
  searchCurrentMatch?: FindMatch | null;
  onFocusCell: (cellId: string) => void;
  onExecuteCell: (cellId: string) => void;
  onInterruptKernel: () => void;
  onDeleteCell: (cellId: string) => void;
  onAddCell: (type: "code" | "markdown", afterCellId?: string | null) => void;
  onMoveCell: (cellId: string, afterCellId?: string | null) => void;
  onClearPagePayload: (cellId: string) => void;
  onReportOutputMatchCount?: (cellId: string, count: number) => void;
  onSetCellSourceHidden?: (cellId: string, hidden: boolean) => void;
  onSetCellOutputsHidden?: (cellId: string, hidden: boolean) => void;
}

const adderRibbonColors: Record<string, { light: string; dark: string }> = {
  code: { light: "rgb(56, 189, 248)", dark: "rgb(2, 132, 199)" },
  markdown: { light: "rgb(52, 211, 153)", dark: "rgb(5, 150, 105)" },
  raw: { light: "rgb(251, 113, 133)", dark: "rgb(225, 29, 72)" },
};
const defaultAdderRibbonColor = adderRibbonColors.code;

function CellAdder({
  afterCellId,
  onAdd,
  cellType = "code",
}: {
  afterCellId?: string | null;
  onAdd: (type: "code" | "markdown", afterCellId?: string | null) => void;
  cellType?: string;
}) {
  const ribbonColor = adderRibbonColors[cellType] ?? defaultAdderRibbonColor;

  return (
    <div className="flex h-7 w-full items-center select-none">
      {/* Hover zone limited to gutter + ribbon area */}
      <div className="group/adder flex h-full flex-shrink-0 items-center pr-3">
        {/* Gutter spacer — matches cell gutter w-10 */}
        <div className="w-10" />
        {/* Ribbon zone — widens on hover to reveal cell type options */}
        <div
          style={
            {
              "--adder-ribbon": ribbonColor.light,
              "--adder-ribbon-dark": ribbonColor.dark,
            } as React.CSSProperties
          }
          className={cn(
            "flex h-full flex-shrink-0 items-center overflow-hidden",
            "w-1 bg-gray-200 transition-all duration-200 ease-out dark:bg-gray-700",
            "group-hover/adder:w-auto group-hover/adder:rounded-r-sm group-hover/adder:bg-[var(--adder-ribbon)] group-hover/adder:pr-1 dark:group-hover/adder:bg-[var(--adder-ribbon-dark)]",
            "group-focus-within/adder:w-auto group-focus-within/adder:rounded-r-sm group-focus-within/adder:bg-[var(--adder-ribbon)] group-focus-within/adder:pr-1 dark:group-focus-within/adder:bg-[var(--adder-ribbon-dark)]",
          )}
        >
          <div
            className={cn(
              "flex items-center gap-0.5 pl-1.5 opacity-0 transition-opacity duration-150",
              "group-hover/adder:opacity-100 group-hover/adder:delay-75",
              "group-focus-within/adder:opacity-100 group-focus-within/adder:delay-75",
            )}
          >
            <button
              type="button"
              title="Add code cell"
              onClick={() => onAdd("code", afterCellId)}
              className="flex items-center whitespace-nowrap rounded-sm px-2 py-0.5 text-xs font-medium text-white/70 transition-colors hover:bg-white/20 hover:text-white"
            >
              + Code
            </button>
            <button
              type="button"
              title="Add markdown cell"
              onClick={() => onAdd("markdown", afterCellId)}
              className="flex items-center whitespace-nowrap rounded-sm px-2 py-0.5 text-xs font-medium text-white/70 transition-colors hover:bg-white/20 hover:text-white"
            >
              + Markdown
            </button>
          </div>
        </div>
      </div>
      {/* Content area — no hover trigger */}
      <div className="flex-1" />
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

/** Index card preview shown when dragging a cell */
function CellDragPreview({ cellId }: { cellId: string }) {
  const cell = useCell(cellId);
  if (!cell) return null;

  // Get first 3 lines of source, truncated
  const sourceLines = cell.source.split("\n").slice(0, 3);
  const hasMoreLines = cell.source.split("\n").length > 3;
  const hasOutputs =
    cell.cell_type === "code" && (cell as CodeCellType).outputs.length > 0;

  // Ribbon color based on cell type
  const ribbonColor =
    cell.cell_type === "code"
      ? "bg-sky-400 dark:bg-sky-500"
      : cell.cell_type === "raw"
        ? "bg-rose-400 dark:bg-rose-500"
        : "bg-emerald-400 dark:bg-emerald-500";

  return (
    <div className="w-80 rounded-lg bg-background shadow-2xl ring-1 ring-border/50 rotate-1 scale-[1.02] overflow-hidden">
      <div className="flex">
        <div className={cn("w-1 flex-shrink-0", ribbonColor)} />
        <div className="flex-1 p-3 min-w-0">
          {sourceLines.length > 0 && sourceLines[0] !== "" ? (
            <pre className="text-xs text-foreground font-mono whitespace-pre overflow-hidden">
              {sourceLines.map((line, i) => (
                <span key={i} className="block truncate">
                  {line || " "}
                </span>
              ))}
            </pre>
          ) : (
            <p className="text-xs text-muted-foreground italic">Empty cell</p>
          )}
          {(hasMoreLines || hasOutputs) && (
            <div className="mt-2 flex items-center gap-2 text-[10px] text-muted-foreground">
              {hasMoreLines && <span>...</span>}
              {hasOutputs && (
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {(cell as CodeCellType).outputs.length} output
                  {(cell as CodeCellType).outputs.length !== 1 ? "s" : ""}
                </span>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/** Check if a cell has both source and outputs hidden via metadata.
 *  We intentionally don't check outputs.length so cells stay collapsed
 *  when outputs are transiently cleared during re-execution. */
function isCellFullyHidden(cell: NotebookCell): boolean {
  if (cell.cell_type !== "code") return false;
  const jupyter = cell.metadata?.jupyter as
    | { source_hidden?: boolean; outputs_hidden?: boolean }
    | undefined;
  return jupyter?.source_hidden === true && jupyter?.outputs_hidden === true;
}

/**
 * Per-cell subscriber component. Uses useCell(id) so it only re-renders
 * when this specific cell changes — not when other cells change.
 */
const CellRenderer = memo(function CellRenderer({
  cellId,
  index,
  renderCell,
  dragHandleProps,
  isDragging,
}: {
  cellId: string;
  index: number;
  renderCell: (
    cell: NotebookCell,
    index: number,
    dragHandleProps?: Record<string, unknown>,
    isDragging?: boolean,
  ) => React.ReactNode;
  dragHandleProps?: Record<string, unknown>;
  isDragging?: boolean;
}) {
  const cell = useCell(cellId);
  if (!cell) return null;
  return <>{renderCell(cell, index, dragHandleProps, isDragging)}</>;
});

/** Wrapper component for sortable cells */
function SortableCell({
  cellId,
  nextCellId,
  index,
  renderCell,
  onAddCell,
  onDeleteCell,
  isHiddenInGroup,
}: {
  cellId: string;
  nextCellId?: string;
  index: number;
  renderCell: (
    cell: NotebookCell,
    index: number,
    dragHandleProps?: Record<string, unknown>,
    isDragging?: boolean,
  ) => React.ReactNode;
  onAddCell: (type: "code" | "markdown", afterCellId?: string | null) => void;
  onDeleteCell: (cellId: string) => void;
  isHiddenInGroup?: boolean;
}) {
  const cell = useCell(cellId);
  const nextCell = useCell(nextCellId ?? "");
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: cellId });

  const style = {
    transform: DndCSS.Transform.toString(transform),
    transition,
  };

  // Combine listeners and attributes for the drag handle
  // This enables keyboard-initiated dragging (Space/Enter + arrows)
  const dragHandleProps = {
    ...listeners,
    ...attributes,
  };

  if (isHiddenInGroup) {
    return <div ref={setNodeRef} style={style} />;
  }

  const cellType = cell?.cell_type ?? "code";
  // Adder color matches the cell below; for the last cell, fall back to its own type
  const nextCellType = nextCell?.cell_type ?? cellType;

  return (
    <div ref={setNodeRef} style={style}>
      {index === 0 && (
        <CellAdder afterCellId={null} onAdd={onAddCell} cellType={cellType} />
      )}
      <ErrorBoundary
        fallback={(error, resetErrorBoundary) => (
          <CellErrorFallback
            error={error}
            onRetry={resetErrorBoundary}
            onDelete={() => onDeleteCell(cellId)}
          />
        )}
      >
        <CellRenderer
          cellId={cellId}
          index={index}
          renderCell={renderCell}
          dragHandleProps={dragHandleProps}
          isDragging={isDragging}
        />
      </ErrorBoundary>
      <CellAdder
        afterCellId={cellId}
        onAdd={onAddCell}
        cellType={nextCellType}
      />
    </div>
  );
}

function NotebookViewContent({
  cellIds,
  isLoading = false,
  focusedCellId,
  executingCellIds,
  queuedCellIds,
  pagePayloads,
  runtime = "python",
  searchQuery,
  searchCurrentMatch,
  onFocusCell,
  onExecuteCell,
  onInterruptKernel,
  onDeleteCell,
  onAddCell,
  onMoveCell,
  onClearPagePayload,
  onReportOutputMatchCount,
  onSetCellSourceHidden,
  onSetCellOutputsHidden,
}: NotebookViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  // Track whether focus change was keyboard-driven (should scroll) or mouse-driven (already visible)
  const focusSourceRef = useRef<"mouse" | "keyboard">("keyboard");
  const { focusCell } = useEditorRegistry();

  // Track full materializations for cross-cell derived state
  const materializeVersion = useMaterializeVersion();

  // Drag-and-drop state
  const [activeId, setActiveId] = useState<string | null>(null);

  // Configure dnd-kit sensors
  const sensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: { distance: 8 },
    }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    }),
  );

  const handleDragStart = useCallback((event: DragStartEvent) => {
    setActiveId(event.active.id as string);
  }, []);

  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const { active, over } = event;
      setActiveId(null);

      if (over && active.id !== over.id) {
        const oldIndex = cellIds.indexOf(active.id as string);
        const newIndex = cellIds.indexOf(over.id as string);

        // Calculate afterCellId: we want to place the cell after the cell
        // that will be above it in the new position
        let afterCellId: string | null;
        if (newIndex === 0) {
          // Moving to the beginning
          afterCellId = null;
        } else if (newIndex > oldIndex) {
          // Moving down: place after the cell at newIndex
          afterCellId = cellIds[newIndex];
        } else {
          // Moving up: place after the cell at newIndex - 1
          afterCellId = newIndex > 0 ? cellIds[newIndex - 1] : null;
        }

        onMoveCell(active.id as string, afterCellId);
      }
    },
    [cellIds, onMoveCell],
  );

  // Compute consecutive groups of fully-hidden cells
  // Maps cell ID → { count, isFirst, groupCellIds }
  // Recomputes on structural changes and full materializations (metadata changes)
  const hiddenGroups = useMemo(() => {
    // Depend on cellIds (structural changes) and materializeVersion
    // (metadata changes like source_hidden) to recompute.
    // We read cells imperatively since this is cross-cell derived state.
    void cellIds;
    void materializeVersion;
    const cells = getNotebookCellsSnapshot();
    const groups = new Map<
      string,
      {
        count: number;
        isFirst: boolean;
        groupCellIds: string[];
        errorCount: number;
      }
    >();
    let i = 0;
    while (i < cells.length) {
      if (isCellFullyHidden(cells[i])) {
        const groupCellIds: string[] = [];
        let groupErrorCount = 0;
        while (i < cells.length && isCellFullyHidden(cells[i])) {
          const c = cells[i];
          groupCellIds.push(c.id);
          if (c.cell_type === "code") {
            groupErrorCount += c.outputs.filter(
              (o) => o.output_type === "error",
            ).length;
          }
          i++;
        }
        for (let j = 0; j < groupCellIds.length; j++) {
          groups.set(groupCellIds[j], {
            count: groupCellIds.length,
            isFirst: j === 0,
            groupCellIds,
            errorCount: groupErrorCount,
          });
        }
      } else {
        i++;
      }
    }
    return groups;
  }, [cellIds, materializeVersion]);

  // Compute the cell ID that precedes the focused cell (keeps its output bright)
  const previousCellId = useMemo(() => {
    if (!focusedCellId) return null;
    const focusedIndex = cellIds.indexOf(focusedCellId);
    return focusedIndex > 0 ? cellIds[focusedIndex - 1] : null;
  }, [focusedCellId, cellIds]);

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

  useEffect(() => {
    if (!focusedCellId) return;
    // Only scroll for keyboard-driven focus changes (arrows, shift-enter).
    // Mouse clicks don't need scrolling — the cell is already visible.
    if (focusSourceRef.current !== "keyboard") {
      focusSourceRef.current = "keyboard"; // reset for next time
      return;
    }
    const cellEl = containerRef.current?.querySelector(
      `[data-cell-id="${CSS.escape(focusedCellId)}"]`,
    );
    if (cellEl) {
      cellEl.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }, [focusedCellId]);

  // ── Auto-seed first cell for empty notebooks ───────────────────────
  // For new notebooks the daemon creates zero cells. Once sync completes
  // (isLoading becomes false), we create the first code cell locally via
  // the CRDT so the user gets an instant focused editor. The ref guard
  // ensures we only seed once even if the effect re-fires before React
  // processes the focusedCellId update from onAddCell.
  const didAutoSeed = useRef(false);
  useEffect(() => {
    if (isLoading || focusedCellId !== null) return;
    if (cellIds.length === 0) {
      if (!didAutoSeed.current) {
        didAutoSeed.current = true;
        onAddCell("code");
      }
    } else {
      onFocusCell(cellIds[0]);
    }
  }, [isLoading, cellIds, focusedCellId, onFocusCell, onAddCell]);

  const renderCell = useCallback(
    (
      cell: NotebookCell,
      index: number,
      dragHandleProps?: Record<string, unknown>,
      isDragging?: boolean,
    ) => {
      const isFocused = cell.id === focusedCellId;
      const isExecuting = executingCellIds.has(cell.id);
      const isQueued = queuedCellIds.has(cell.id);

      // Navigation callbacks — skip cells that are collapsed into a hidden group
      const isVisibleCell = (id: string) => {
        const g = hiddenGroups.get(id);
        return !g || g.isFirst;
      };

      const onFocusPrevious = (cursorPosition: "start" | "end") => {
        logger.debug(
          `[cell-nav] onFocusPrevious called: cell=${cell.id.slice(0, 8)} index=${index} cellIds=${cellIds.map((id) => id.slice(0, 8)).join(",")}`,
        );
        focusSourceRef.current = "keyboard";
        let prevIndex = index - 1;
        while (prevIndex >= 0 && !isVisibleCell(cellIds[prevIndex])) {
          prevIndex--;
        }
        if (prevIndex >= 0) {
          const prevCellId = cellIds[prevIndex];
          logger.debug(
            `[cell-nav] Focusing previous: ${prevCellId.slice(0, 8)}`,
          );
          onFocusCell(prevCellId);
          focusCell(prevCellId, cursorPosition);
        } else {
          logger.debug("[cell-nav] No previous cell (index=0)");
        }
      };

      const onFocusNext = (cursorPosition: "start" | "end") => {
        logger.debug(
          `[cell-nav] onFocusNext called: cell=${cell.id.slice(0, 8)} index=${index} cellIds=${cellIds.map((id) => id.slice(0, 8)).join(",")}`,
        );
        focusSourceRef.current = "keyboard";
        let nextIndex = index + 1;
        while (
          nextIndex < cellIds.length &&
          !isVisibleCell(cellIds[nextIndex])
        ) {
          nextIndex++;
        }
        if (nextIndex < cellIds.length) {
          const nextCellId = cellIds[nextIndex];
          logger.debug(`[cell-nav] Focusing next: ${nextCellId.slice(0, 8)}`);
          onFocusCell(nextCellId);
          focusCell(nextCellId, cursorPosition);
        } else {
          logger.debug("[cell-nav] No next cell (at end)");
        }
      };

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
            isQueued={isQueued}
            pagePayload={pagePayload}
            searchQuery={searchQuery}
            searchActiveOffset={activeSourceOffset}
            onSearchMatchCount={
              onReportOutputMatchCount
                ? (count: number) => onReportOutputMatchCount(cell.id, count)
                : undefined
            }
            onFocus={() => {
              focusSourceRef.current = "mouse";
              onFocusCell(cell.id);
            }}
            onExecute={() => onExecuteCell(cell.id)}
            onInterrupt={onInterruptKernel}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onInsertCellAfter={() => onAddCell("code", cell.id)}
            onClearPagePayload={() => onClearPagePayload(cell.id)}
            isLastCell={index === cellIds.length - 1}
            dragHandleProps={dragHandleProps}
            isDragging={isDragging}
            onToggleSourceHidden={
              onSetCellSourceHidden
                ? (hidden: boolean) => onSetCellSourceHidden(cell.id, hidden)
                : undefined
            }
            onToggleOutputsHidden={
              onSetCellOutputsHidden
                ? (hidden: boolean) => onSetCellOutputsHidden(cell.id, hidden)
                : undefined
            }
            hiddenGroupCount={hiddenGroups.get(cell.id)?.count}
            hiddenGroupErrorCount={hiddenGroups.get(cell.id)?.errorCount}
            isGroupExecuting={
              hiddenGroups
                .get(cell.id)
                ?.groupCellIds.some((id) => executingCellIds.has(id)) ?? false
            }
            onExpandHiddenGroup={
              hiddenGroups.has(cell.id) &&
              onSetCellSourceHidden &&
              onSetCellOutputsHidden
                ? () => {
                    const group = hiddenGroups.get(cell.id);
                    if (group) {
                      for (const id of group.groupCellIds) {
                        onSetCellSourceHidden(id, false);
                        onSetCellOutputsHidden(id, false);
                      }
                    }
                  }
                : undefined
            }
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
            onFocus={() => {
              focusSourceRef.current = "mouse";
              onFocusCell(cell.id);
            }}
            onDelete={() => onDeleteCell(cell.id)}
            onFocusPrevious={onFocusPrevious}
            onFocusNext={onFocusNext}
            onInsertCellAfter={() => onAddCell("markdown", cell.id)}
            isLastCell={index === cellIds.length - 1}
            dragHandleProps={dragHandleProps}
            isDragging={isDragging}
          />
        );
      }

      // Raw cells
      return (
        <RawCell
          key={cell.id}
          cell={cell}
          isFocused={isFocused}
          isPreviousCellFromFocused={cell.id === previousCellId}
          searchQuery={searchQuery}
          onFocus={() => {
            focusSourceRef.current = "mouse";
            onFocusCell(cell.id);
          }}
          onDelete={() => onDeleteCell(cell.id)}
          onFocusPrevious={onFocusPrevious}
          onFocusNext={onFocusNext}
          onInsertCellAfter={() => onAddCell("code", cell.id)}
          isLastCell={index === cellIds.length - 1}
          dragHandleProps={dragHandleProps}
          isDragging={isDragging}
        />
      );
    },
    [
      focusedCellId,
      previousCellId,
      executingCellIds,
      queuedCellIds,
      pagePayloads,
      runtime,
      searchQuery,
      searchCurrentMatch,
      cellIds,
      onFocusCell,
      onExecuteCell,
      onInterruptKernel,
      onDeleteCell,
      onAddCell,
      onClearPagePayload,
      onReportOutputMatchCount,
      onSetCellSourceHidden,
      onSetCellOutputsHidden,
      hiddenGroups,
      focusCell,
    ],
  );

  return (
    <div
      ref={containerRef}
      className="flex-1 overflow-y-auto overflow-x-clip overscroll-x-contain py-4 pl-8 pr-4"
      style={{ contain: "paint" }}
      data-notebook-synced={!isLoading && cellIds.length > 0}
      data-cell-count={cellIds.length}
    >
      {cellIds.length === 0 ? (
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
          collisionDetection={closestCenter}
          onDragStart={handleDragStart}
          onDragEnd={handleDragEnd}
          onDragCancel={() => setActiveId(null)}
        >
          <SortableContext
            items={cellIds}
            strategy={verticalListSortingStrategy}
          >
            {cellIds.map((cellId, index) => {
              const group = hiddenGroups.get(cellId);
              return (
                <SortableCell
                  key={cellId}
                  cellId={cellId}
                  nextCellId={cellIds[index + 1]}
                  index={index}
                  renderCell={renderCell}
                  onAddCell={onAddCell}
                  onDeleteCell={onDeleteCell}
                  isHiddenInGroup={group != null && !group.isFirst}
                />
              );
            })}
          </SortableContext>
          <DragOverlay>
            {activeId && <CellDragPreview cellId={activeId} />}
          </DragOverlay>
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
