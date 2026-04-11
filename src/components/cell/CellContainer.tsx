import { forwardRef, type ReactNode } from "react";
import { cn } from "@/lib/utils";
import { type GutterColorConfig, getGutterColors } from "./gutter-colors";

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
  /** Whether this cell is immediately after the focused cell (keeps output bright) */
  isNextCellFromFocused?: boolean;
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
      isNextCellFromFocused = false,
      dragHandleProps,
      isDragging = false,
      className,
    },
    ref,
  ) => {
    const colors = getGutterColors(cellType, customGutterColors);
    const ribbonColor = isFocused ? colors.ribbon.focused : colors.ribbon.default;
    const outputRibbonColor = isFocused ? colors.outputRibbon.focused : colors.outputRibbon.default;
    const bgColor = isFocused ? colors.background.focused : undefined;

    // Use segmented ribbon when codeContent is provided
    const useSegmentedRibbon = codeContent !== undefined;
    const hasOutput = outputContent !== undefined && outputContent !== null;

    return (
      <div
        ref={ref}
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
      >
        {/* Gutter area - action content only (ribbon moves to content rows for segmented) */}
        <div
          className="flex w-10 flex-shrink-0 flex-col items-end justify-start gap-0.5 pr-1 pt-3.5 select-none"
          onMouseDown={onFocus}
        >
          {gutterContent}
          {presenceIndicators}
        </div>
        {/* Cell content with ribbon */}
        {useSegmentedRibbon ? (
          <div className="flex min-w-0 flex-1 flex-col">
            {/* Code row - ribbon + content + right gutter */}
            <div className="flex" onMouseDown={onFocus}>
              <div
                {...dragHandleProps}
                className={cn(
                  "w-1 transition-colors duration-150",
                  ribbonColor,
                  dragHandleProps && "cursor-grab hover:brightness-125 touch-none",
                  isDragging && "cursor-grabbing",
                )}
              />
              <div className="min-w-0 flex-1 pt-1.5 pb-3 pl-6 pr-3">{codeContent}</div>
              {/* Code row right gutter — always rendered as spacer for consistent width */}
              <div
                className={cn(
                  "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-1 select-none",
                  rightGutterContent && "opacity-100 transition-opacity duration-150",
                  rightGutterContent &&
                    "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                  rightGutterContent && isFocused && "sm:opacity-100",
                )}
              >
                {rightGutterContent}
              </div>
            </div>
            {/* Output row - ribbon + content + right gutter
                onMouseDown sets visual focus (ribbon/bg) without stealing editor focus */}
            {hasOutput && (
              <div className={cn("flex", hideOutput && "hidden")} onMouseDown={onFocus}>
                <div className={cn("w-1 transition-colors duration-150", outputRibbonColor)} />
                <div
                  className={cn(
                    "min-w-0 flex-1 py-2 transition-opacity duration-150",
                    !isFocused &&
                      !isPreviousCellFromFocused &&
                      !isNextCellFromFocused &&
                      "opacity-70",
                  )}
                >
                  {outputContent}
                </div>
                {/* Output row right gutter — always rendered as spacer for consistent width */}
                <div
                  className={cn(
                    "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-1 select-none",
                    outputRightGutterContent && "opacity-100 transition-opacity duration-150",
                    outputRightGutterContent &&
                      "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                    outputRightGutterContent && isFocused && "sm:opacity-100",
                  )}
                >
                  {outputRightGutterContent}
                </div>
              </div>
            )}
          </div>
        ) : (
          <>
            {/* Legacy layout - ribbon + content side by side */}
            <div className="flex min-w-0 flex-1" onMouseDown={onFocus}>
              <div
                {...dragHandleProps}
                className={cn(
                  "w-1 self-stretch transition-colors duration-150",
                  ribbonColor,
                  dragHandleProps && "cursor-grab hover:brightness-125 touch-none",
                  isDragging && "cursor-grabbing",
                )}
              />
              <div className="min-w-0 flex-1 pt-1.5 pb-3 pl-6 pr-3">{children}</div>
            </div>
            {/* Right margin for legacy layout — always rendered as spacer */}
            <div
              className={cn(
                "flex w-10 flex-shrink-0 flex-col items-center gap-1 pt-3 select-none",
                rightGutterContent && "opacity-100 transition-opacity duration-150",
                rightGutterContent &&
                  "sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100",
                rightGutterContent && isFocused && "sm:opacity-100",
              )}
            >
              {rightGutterContent}
            </div>
          </>
        )}
      </div>
    );
  },
);

CellContainer.displayName = "CellContainer";
