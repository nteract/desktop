import { cn } from "@/lib/utils";

interface ExecutionCountProps {
  count: number | null;
  isExecuting?: boolean;
  className?: string;
}

export function ExecutionCount({ count, isExecuting, className }: ExecutionCountProps) {
  const display = isExecuting ? "*" : (count ?? " ");
  return (
    <span
      data-slot="execution-count"
      className={cn("font-mono text-sm tabular-nums text-muted-foreground", className)}
    >
      [{display}]:
    </span>
  );
}
