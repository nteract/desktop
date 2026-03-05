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
  /** Content to render in the right margin (e.g., cell controls, kebab menu) */
  rightGutterContent?: ReactNode;
  /** Custom color configuration for cell types not in defaults */
  customGutterColors?: Record<string, GutterColorConfig>;
  onDragStart?: (e: React.DragEvent) => void;
  onDragOver?: (e: React.DragEvent) => void;
  onDrop?: (e: React.DragEvent) => void;
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
      customGutterColors,
      onDragStart,
      onDragOver,
      onDrop,
      className,
    },
    ref,
  ) => {
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
        ref={ref}
        data-slot="cell-container"
        data-cell-id={id}
        data-cell-type={cellType}
        className={cn(
          "cell-container group flex transition-colors duration-150",
          bgColor,
          isFocused && "-mx-16 px-16",
          className,
        )}
        onMouseDown={onFocus}
        draggable={!!onDragStart}
        onDragStart={onDragStart}
        onDragOver={onDragOver}
        onDrop={onDrop}
      >
        {/* Gutter area - action content only (ribbon moves to content rows for segmented) */}
        <div className="flex w-10 flex-shrink-0 flex-col items-end justify-start gap-0.5 pr-1 pt-3 select-none">
          {gutterContent}
        </div>
        {/* Cell content with ribbon */}
        {useSegmentedRibbon ? (
          <div className="flex min-w-0 flex-1 flex-col">
            {/* Code row - ribbon + content together so heights match */}
            <div className="flex">
              <div
                className={cn(
                  "w-1 transition-colors duration-150",
                  ribbonColor,
                )}
              />
              <div className="min-w-0 flex-1 py-3 pl-6 pr-3">{codeContent}</div>
            </div>
            {/* Output row - ribbon + content together */}
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
                    !isFocused && "opacity-70",
                  )}
                >
                  {outputContent}
                </div>
              </div>
            )}
          </div>
        ) : (
          /* Legacy layout - ribbon + content side by side */
          <div className="flex min-w-0 flex-1">
            <div
              className={cn(
                "w-1 self-stretch transition-colors duration-150",
                ribbonColor,
              )}
            />
            <div className="min-w-0 flex-1 py-3 pl-6 pr-3">{children}</div>
          </div>
        )}
        {/* Right margin - pt-3 aligns with left gutter, appears on hover/focus */}
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
      </div>
    );
  },
);

CellContainer.displayName = "CellContainer";
