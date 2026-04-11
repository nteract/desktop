import { Circle, Terminal } from "lucide-react";
import type React from "react";
import { cn } from "@/lib/utils";

export type RuntimeStatus = "idle" | "busy" | "disconnected" | "connecting";

interface RuntimeHealthIndicatorProps {
  status: RuntimeStatus;
  kernelName?: string;
  onClick?: () => void;
  showStatus?: boolean;
  className?: string;
}

export function getStatusColor(status: RuntimeStatus): string {
  switch (status) {
    case "idle":
      return "text-green-500";
    case "busy":
      return "text-amber-500";
    case "connecting":
      return "text-blue-500";
    default:
      return "text-red-500";
  }
}

export function getStatusText(status: RuntimeStatus): string {
  switch (status) {
    case "idle":
      return "Connected";
    case "busy":
      return "Busy";
    case "connecting":
      return "Connecting...";
    default:
      return "Disconnected";
  }
}

export function getStatusTextColor(status: RuntimeStatus): string {
  switch (status) {
    case "idle":
      return "text-green-600";
    case "busy":
      return "text-amber-600";
    case "connecting":
      return "text-blue-600";
    default:
      return "text-red-600";
  }
}

export const RuntimeHealthIndicator: React.FC<RuntimeHealthIndicatorProps> = ({
  status,
  kernelName,
  onClick,
  showStatus = false,
  className,
}) => {
  const content = (
    <>
      {kernelName && (
        <>
          <Terminal className="h-3 w-3 sm:h-4 sm:w-4" />
          <span className="hidden text-xs sm:block sm:text-sm">{kernelName}</span>
        </>
      )}
      <Circle className={cn("size-2 fill-current", getStatusColor(status))} />
      {showStatus && (
        <span className={cn("text-xs", getStatusTextColor(status))}>{getStatusText(status)}</span>
      )}
    </>
  );

  if (onClick) {
    return (
      <button
        type="button"
        data-slot="runtime-health-indicator"
        onClick={onClick}
        className={cn(
          "flex items-center gap-1 rounded-md border border-input bg-background px-2 py-1 text-sm transition-colors hover:bg-accent hover:text-accent-foreground sm:gap-2",
          className,
        )}
      >
        {content}
      </button>
    );
  }

  return (
    <div
      data-slot="runtime-health-indicator"
      className={cn("flex items-center gap-1 sm:gap-2", className)}
    >
      {content}
    </div>
  );
};
