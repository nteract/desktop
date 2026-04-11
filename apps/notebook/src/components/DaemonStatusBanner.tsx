import { AlertTriangle, Loader2, RefreshCw, X } from "lucide-react";

/**
 * Status of the daemon during startup or operation.
 * Matches the DaemonProgress enum from Rust.
 */
export type DaemonStatus =
  | { status: "checking" }
  | { status: "installing" }
  | { status: "upgrading" }
  | { status: "starting" }
  | { status: "waiting_for_ready"; attempt: number; max_attempts: number }
  | { status: "ready"; endpoint: string }
  | { status: "failed"; error: string; guidance?: string }
  | null;

interface DaemonStatusBannerProps {
  status: DaemonStatus;
  onDismiss?: () => void;
  onRetry?: () => void;
}

/**
 * Banner component showing daemon startup progress or errors.
 *
 * Shows different visual states:
 * - Blue/info with spinner: Installing, upgrading, starting, waiting
 * - Amber/warning: Failed state with retry button
 * - Hidden: Ready state or null
 */
export function DaemonStatusBanner({ status, onDismiss, onRetry }: DaemonStatusBannerProps) {
  // Don't show banner for ready or null state
  if (!status || status.status === "ready") {
    return null;
  }

  // Failed state - amber banner with error message and retry button
  if (status.status === "failed") {
    return (
      <div className="flex flex-col gap-1 bg-amber-600/90 px-3 py-2 text-xs text-white">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <AlertTriangle className="h-3 w-3 flex-shrink-0" />
            <span className="font-medium flex-shrink-0">Runtime unavailable</span>
            <span className="text-amber-200 flex-shrink-0">—</span>
            <span className="text-amber-100 truncate">{status.error}</span>
          </div>
          <div className="flex items-center gap-1 flex-shrink-0">
            {onRetry && (
              <button
                type="button"
                onClick={onRetry}
                className="flex items-center gap-1 rounded px-2 py-0.5 hover:bg-amber-500/50 transition-colors"
              >
                <RefreshCw className="h-3 w-3" />
                <span>Retry</span>
              </button>
            )}
            {onDismiss && (
              <button
                type="button"
                onClick={onDismiss}
                className="rounded p-0.5 hover:bg-amber-500/50 transition-colors"
                aria-label="Dismiss"
              >
                <X className="h-3 w-3" />
              </button>
            )}
          </div>
        </div>
        {status.guidance && <div className="text-amber-100 pl-5">{status.guidance}</div>}
      </div>
    );
  }

  // Progress states - soft blue banner with spinner
  const message = getProgressMessage(status);

  return (
    <div className="flex items-center gap-2 bg-sky-600/90 px-3 py-1 text-xs text-white">
      <Loader2 className="h-3 w-3 animate-spin flex-shrink-0" />
      <span>{message}</span>
    </div>
  );
}

function getProgressMessage(
  status: Exclude<DaemonStatus, null | { status: "ready" } | { status: "failed" }>,
): string {
  switch (status.status) {
    case "checking":
      return "Checking runtime status...";
    case "installing":
      return "Installing runtime (first launch)...";
    case "upgrading":
      return "Upgrading runtime...";
    case "starting":
      return "Starting runtime...";
    case "waiting_for_ready":
      return `Starting runtime (${status.attempt}/${status.max_attempts})...`;
  }
}
