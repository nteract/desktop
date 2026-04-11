import { Loader2, Play, Square } from "lucide-react";
import type React from "react";
import { cn } from "@/lib/utils";

interface PlayButtonProps {
  executionState: "idle" | "queued" | "running" | "completed" | "error";
  cellType: string;
  isFocused?: boolean;
  onExecute: () => void;
  onInterrupt: () => void;
  className?: string;
  focusedClass?: string;
  isAutoLaunching?: boolean;
  /**
   * When true, uses gutter-optimized visibility (invisible when unfocused,
   * visible on focus or parent hover). Default: false for backwards compatibility.
   */
  gutterMode?: boolean;
}

export const PlayButton: React.FC<PlayButtonProps> = ({
  executionState,
  cellType,
  isFocused = false,
  onExecute,
  onInterrupt,
  className = "",
  focusedClass = "text-foreground",
  isAutoLaunching = false,
  gutterMode = false,
}) => {
  const isRunning = executionState === "running" || executionState === "queued";
  const title = isAutoLaunching
    ? "Starting runtime..."
    : isRunning
      ? "Stop execution"
      : `Execute ${cellType} cell`;

  // Visibility classes differ based on mode
  const visibilityClass = gutterMode
    ? isRunning
      ? "text-destructive hover:text-destructive animate-pulse"
      : isFocused
        ? focusedClass
        : "text-transparent group-hover:text-muted-foreground hover:text-foreground"
    : isRunning
      ? "text-destructive hover:text-destructive shadow-destructive/20 animate-pulse drop-shadow-sm"
      : isFocused
        ? focusedClass
        : "text-muted-foreground/40 hover:text-foreground group-hover:text-foreground";

  return (
    <button
      data-slot="play-button"
      onClick={isRunning ? onInterrupt : onExecute}
      disabled={isAutoLaunching}
      className={cn(
        "flex items-center justify-center transition-all",
        gutterMode
          ? "rounded-sm p-0.5" // Minimal padding in gutter mode
          : "hover:bg-muted/80 rounded-sm bg-background p-1",
        visibilityClass,
        isAutoLaunching && "cursor-wait opacity-75",
        className,
      )}
      title={title}
    >
      {isAutoLaunching ? (
        <Loader2 className={cn(gutterMode ? "size-3.5" : "size-4", "animate-spin")} />
      ) : isRunning ? (
        <Square
          fill={gutterMode ? "currentColor" : "none"}
          stroke={gutterMode ? "none" : "currentColor"}
          strokeWidth={gutterMode ? undefined : "2"}
          className={gutterMode ? "size-2.5" : "size-4"}
        />
      ) : (
        <Play
          fill="currentColor"
          stroke="none"
          className={cn(gutterMode ? "size-3.5" : "size-4")}
        />
      )}
    </button>
  );
};
