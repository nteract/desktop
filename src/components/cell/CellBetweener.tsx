import { cn } from "@/lib/utils";
import { type GutterColorConfig, getGutterColors } from "./gutter-colors";

interface CellBetweenerProps {
  /** Cell type to determine ribbon color - defaults to "code" */
  cellType?: string;
  /** Custom color configuration */
  customGutterColors?: Record<string, GutterColorConfig>;
  /** Optional content for the betweener area (e.g., add cell button) */
  children?: React.ReactNode;
  className?: string;
}

/**
 * A spacer component that maintains gutter ribbon continuity between cells.
 * Place this between CellContainer components to create an unbroken
 * "paper edge" effect down the left side of the notebook.
 */
export function CellBetweener({
  cellType = "code",
  customGutterColors,
  children,
  className,
}: CellBetweenerProps) {
  const colors = getGutterColors(cellType, customGutterColors);

  return (
    <div data-slot="cell-betweener" className={cn("flex h-4 w-full items-center", className)}>
      {/* Gutter spacer - matches cell gutter structure */}
      <div className="flex h-full flex-shrink-0">
        {/* Action area spacer - matches CellContainer w-10 */}
        <div className="w-10" />
        {/* Ribbon continues */}
        <div className={cn("w-1", colors.ribbon.default)} />
      </div>
      {/* Content area - could hold add-cell buttons */}
      <div className="flex-1">{children}</div>
    </div>
  );
}
