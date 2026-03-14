import {
  forwardRef,
  type ReactNode,
  useEffect,
  useImperativeHandle,
  useRef,
} from "react";
import { cn } from "@/lib/utils";
import { type GutterColorConfig, getGutterColors } from "./gutter-colors";
import { scrollElementIntoViewIfNeeded } from "./scroll-into-view-if-needed";

interface CellContainerProps {
  id: string;
  cellType: string;
  isFocused?: boolean;
  onFocus?: () => void;
  /** Content for the code/editor section (use with outputContent for segmented ribbon) */
  codeContent?: ReactNode;
  /** Content for the output section (renders with a different ribbon color) */
  outputContent?: ReactNode;
  /** Hide the output section (useful for preloading content invisibly) */
  hideOutput?: boolean;
  /** Legacy children prop - use codeContent/outputContent for segmented ribbon support */
  children?: ReactNode;
  /** Content to render in the left gutter action area (e.g., play button, execution count) */
  gutterContent?: ReactNode;
  /** Content to render in the right margin aligned with code row (e.g., cell controls) */
  rightGutterContent?: ReactNode;
  /** Content to render in the right margin aligned with output row (e.g., output controls) */
  outputRightGutterContent?: ReactNode;
  /** Remote peer presence indicators (colored dots showing who's on this cell) */
  presenceIndicators?: ReactNode;
  /** Custom color configuration for cell types not in defaults */
  customGutterColors?: Record<string, GutterColorConfig>;
  /** Whether this cell is immediately before the focused cell (keeps output bright) */
  isPreviousCellFromFocused?: boolean;
  /** Props for dnd-kit drag handle (applied to ribbon) */
  dragHandleProps?: Record<string, unknown>;
  /** Whether this cell is currently being dragged */
  isDragging?: boolean;
  className?: string;
}

export const CellContainer = forwardRef<HTMLDivElement, CellContainerProps>(
  (
    {
      id,
      cellType,
      isFocused = false,
      onFocus,
      codeContent,
      outputContent,
      hideOutput,
      children,
      gutterContent,
      rightGutterContent,
      outputRightGutterContent,
      presenceIndicators,
      customGutterColors,
      isPreviousCellFromFocused = false,
      dragHandleProps,
      isDragging = false,
      className,
    },
    ref,
  ) => {
    const cellRef = useRef<HTMLDivElement | null>(null);
    const previousFocusedRef = useRef<boolean | undefined>(undefined);

    useImperativeHandle(ref, () => cellRef.current as HTMLDivElement, []);

    useEffect(() => {
      const previouslyFocused = previousFocusedRef.current;
      previousFocusedRef.current = isFocused;

      if (!isFocused || previouslyFocused === isFocused) {
        return;
      }

      const cellElement = cellRef.current;
      const hovered =
        cellElement?.parentElement?.querySelector(":hover") === cellElement;

      if (!cellElement || hovered) {
        return;
      }

      const frameId = requestAnimationFrame(() => {
        scrollElementIntoViewIfNeeded(cellElement);
      });

      return () => cancelAnimationFrame(frameId);
    }, [isFocused]);

    const colors = getGutterColors(cellType, customGutterColors);
    const ribbonColor = isFocused
      ? colors.ribbon.focused
      : colors.ribbon.default;
    const outputRibbonColor = isFocused
      ? colors.outputRibbon.focused
      : colors.outputRibbon.default;
    const bgColor = isFocused ? colors.background.focused : undefined;

    // Use segmented ribbon when codeContent is provided
    const useSegmentedRibbon = codeContent !== undefined;
    const hasOutput = outputContent !== undefined && outputContent !== null;

    return (
      <div
        ref={cellRef}
        data-slot="cell-container"
        data-cell-id={id}
        data-cell-type={cellType}
        className={cn(
          "cell-container group flex transition-colors duration-150",
          bgColor,
          isFocused && "-mx-16 px-16",
          isDragging && "opacity-50",
          className,
        )}
        onMouseDown={onFocus}
      >
        {/* Gutter area - action content only (ribbon moves to content rows for segmented) */}
        <div className="flex w-10 flex-shrink-0 flex-col items-end justify-start gap-0.5 pr-1 pt-3 select-none">
          {gutterContent}
          {presenceIndicators}
        </div>
        {/* Cell content with ribbon */}
        {useSegmentedRibbon ? (
          <div className="flex min-w-0 flex-1 flex-col">
            {/* Code row - ribbon + content + right gutter */}
            <div className="flex">
              <div
                {...dragHandleProps}
                className={cn(
                  "w-1 transition-colors duration-150",
                  ribbonColor,
                  dragHandleProps &&
                    "cursor-grab hover:brightness-125 touch-none",
                  isDragging && "cursor-grabbing",
                )}
              />
              <div className="min-w-0 flex-1 py-3 pl-6 pr-3">{codeContent}</div>
              {/* Code row right gutter */}
              {rightGutterContent && (
                <div
                  className={cn(
                    "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-1 select-none",
                    "opacity-100 transition-opacity duration-150",
                    "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                    isFocused && "sm:opacity-100",
                  )}
                >
                  {rightGutterContent}
                </div>
              )}
            </div>
            {/* Output row - ribbon + content + right gutter */}
            {hasOutput && (
              <div className={cn("flex", hideOutput && "hidden")}>
                <div
                  className={cn(
                    "w-1 transition-colors duration-150",
                    outputRibbonColor,
                  )}
                />
                <div
                  className={cn(
                    "min-w-0 flex-1 py-2 pl-6 pr-3 transition-opacity duration-150",
                    !isFocused && !isPreviousCellFromFocused && "opacity-70",
                  )}
                >
                  {outputContent}
                </div>
                {/* Output row right gutter */}
                {outputRightGutterContent && (
                  <div
                    className={cn(
                      "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-1 select-none",
                      "opacity-100 transition-opacity duration-150",
                      "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                      isFocused && "sm:opacity-100",
                    )}
                  >
                    {outputRightGutterContent}
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          <>
            {/* Legacy layout - ribbon + content side by side */}
            <div className="flex min-w-0 flex-1">
              <div
                {...dragHandleProps}
                className={cn(
                  "w-1 self-stretch transition-colors duration-150",
                  ribbonColor,
                  dragHandleProps &&
                    "cursor-grab hover:brightness-125 touch-none",
                  isDragging && "cursor-grabbing",
                )}
              />
              <div className="min-w-0 flex-1 py-3 pl-6 pr-3">{children}</div>
            </div>
            {/* Right margin for legacy layout */}
            {rightGutterContent && (
              <div
                className={cn(
                  "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-3 select-none",
                  "opacity-100 transition-opacity duration-150",
                  "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                  isFocused && "sm:opacity-100",
                )}
              >
                {rightGutterContent}
              </div>
            )}
          </>
        )}
      </div>
    );
  },
);

CellContainer.displayName = "CellContainer";
