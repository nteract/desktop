import type React from "react";
import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface CellHeaderProps {
  className?: string;
  onKeyDown?: (e: React.KeyboardEvent<HTMLElement>) => void;
  onDragStart?: (e: React.DragEvent) => void;
  draggable?: boolean;
  leftContent: ReactNode;
  rightContent: ReactNode;
}

export const CellHeader: React.FC<CellHeaderProps> = ({
  className,
  onKeyDown,
  onDragStart,
  draggable,
  leftContent,
  rightContent,
}) => {
  return (
    <div
      data-slot="cell-header"
      className={cn("cell-header flex items-center justify-between px-1 py-2 sm:pr-4", className)}
      onKeyDown={onKeyDown}
      draggable={draggable}
      onDragStart={onDragStart}
    >
      <div className="flex items-center gap-1">{leftContent}</div>
      {rightContent}
    </div>
  );
};
