import { cn } from "@/lib/utils";

interface CompactExecutionButtonProps {
  /** Execution count - null means never executed */
  count: number | null;
  /** Whether the cell is currently executing */
  isExecuting?: boolean;
  /** Whether the cell is queued for execution */
  isQueued?: boolean;
  /** Called when user clicks to execute */
  onExecute?: () => void;
  /** Called when user clicks to interrupt */
  onInterrupt?: () => void;
  /** Additional classes */
  className?: string;
}

/**
 * Compact execution button combining play + execution count into one element.
 *
 * - Never run: `[ ▶ ]` - click to execute
 * - Queued: `[·]` - breathing dot, waiting in execution queue
 * - Running: `[■]` with pulse - click to stop
 * - Executed: `[1]` - hover to show play, click to re-run
 */
export function CompactExecutionButton({
  count,
  isExecuting = false,
  isQueued = false,
  onExecute,
  onInterrupt,
  className,
}: CompactExecutionButtonProps) {
  const handleClick = () => {
    if (isQueued) return; // already in queue — no-op
    if (isExecuting) {
      onInterrupt?.();
    } else {
      onExecute?.();
    }
  };

  return (
    <button
      type="button"
      onClick={handleClick}
      className={cn(
        "group/exec inline-flex items-center font-mono text-sm tabular-nums",
        "text-muted-foreground hover:text-foreground",
        "transition-colors duration-150",
        isQueued ? "cursor-default" : "cursor-pointer",
        className,
      )}
      title={isExecuting ? "Stop execution" : isQueued ? "Queued for execution" : "Run cell"}
      aria-disabled={isQueued || undefined}
      data-testid="execute-button"
    >
      <span className="opacity-60">[</span>
      <span className="relative inline-flex min-w-4 items-center justify-center">
        {isExecuting ? (
          // Running state: squish-breathe stop indicator with anticipation +
          // overshoot. 1s delay so quick runs stay static.
          <span className="text-destructive animate-exec-squish">■</span>
        ) : isQueued ? (
          // Queued state: small dot with slow breathe animation
          <span className="flex items-center justify-center">
            <span className="block h-1.5 w-1.5 rounded-full bg-muted-foreground animate-queue-breathe" />
          </span>
        ) : count !== null ? (
          // Has count: show count, play on hover
          <>
            <span className="group-hover/exec:opacity-0 transition-opacity">{count}</span>
            <span className="absolute inset-0 flex items-center justify-center opacity-0 group-hover/exec:opacity-100 transition-opacity">
              ▶
            </span>
          </>
        ) : (
          // Never run: show play
          <span>▶</span>
        )}
      </span>
      <span className="opacity-60">]:</span>
    </button>
  );
}
