import { AlertTriangle, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { PoolErrorWithTimestamp } from "../hooks/usePoolState";

interface PoolErrorItemProps {
  envType: "UV" | "Conda";
  error: PoolErrorWithTimestamp;
  onDismiss: () => void;
}

function PoolErrorItem({ envType, error, onDismiss }: PoolErrorItemProps) {
  // Live countdown timer
  const [secondsRemaining, setSecondsRemaining] = useState(() => {
    const elapsed = Math.floor((Date.now() - error.receivedAt) / 1000);
    return Math.max(0, error.retry_in_secs - elapsed);
  });

  useEffect(() => {
    // Don't start interval if already at 0
    const elapsed = Math.floor((Date.now() - error.receivedAt) / 1000);
    const remaining = Math.max(0, error.retry_in_secs - elapsed);
    setSecondsRemaining(remaining);

    if (remaining === 0) return;

    const interval = setInterval(() => {
      const elapsed = Math.floor((Date.now() - error.receivedAt) / 1000);
      const remaining = Math.max(0, error.retry_in_secs - elapsed);
      setSecondsRemaining(remaining);

      // Clear interval once countdown hits 0
      if (remaining === 0) {
        clearInterval(interval);
      }
    }, 1000);

    return () => clearInterval(interval);
  }, [error.receivedAt, error.retry_in_secs]);

  const retryText =
    secondsRemaining > 0 ? `Retrying in ${secondsRemaining}s` : "Retrying...";

  return (
    <div className="flex items-center justify-between gap-2 bg-amber-600/90 px-3 py-1.5 text-xs text-white">
      <div className="flex items-center gap-2 min-w-0">
        <AlertTriangle className="h-3 w-3 flex-shrink-0" />
        <span className="font-medium flex-shrink-0">{envType} pool error</span>
        <span className="text-amber-200 flex-shrink-0">—</span>
        {error.failed_package && (
          <>
            <code className="bg-amber-700/50 px-1 rounded text-amber-100 flex-shrink-0">
              {error.failed_package}
            </code>
            <span className="text-amber-200 flex-shrink-0">—</span>
          </>
        )}
        <span className="text-amber-100 truncate">{error.message}</span>
      </div>
      <div className="flex items-center gap-2 flex-shrink-0">
        <span className="text-amber-200 text-[10px]">{retryText}</span>
        <button
          type="button"
          onClick={onDismiss}
          className="rounded p-0.5 hover:bg-amber-500/50 transition-colors"
          aria-label="Dismiss"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}

interface PoolErrorBannerProps {
  uvError: PoolErrorWithTimestamp | null;
  condaError: PoolErrorWithTimestamp | null;
  onDismissUv: () => void;
  onDismissConda: () => void;
}

/**
 * Banner component showing pool warming errors.
 *
 * Displays amber warning banners for UV and/or Conda pool errors,
 * with the failed package name, error message, and live retry countdown.
 */
export function PoolErrorBanner({
  uvError,
  condaError,
  onDismissUv,
  onDismissConda,
}: PoolErrorBannerProps) {
  if (!uvError && !condaError) {
    return null;
  }

  return (
    <div className="flex flex-col">
      {uvError && (
        <PoolErrorItem envType="UV" error={uvError} onDismiss={onDismissUv} />
      )}
      {condaError && (
        <PoolErrorItem
          envType="Conda"
          error={condaError}
          onDismiss={onDismissConda}
        />
      )}
    </div>
  );
}
