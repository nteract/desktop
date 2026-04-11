import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Clock, Settings, X } from "lucide-react";
import type { PoolErrorWithTimestamp } from "../hooks/usePoolState";

interface PoolErrorItemProps {
  envType: "UV" | "Conda";
  error: PoolErrorWithTimestamp;
  onDismiss: () => void;
}

function errorSubtitle(error: PoolErrorWithTimestamp, envType: "UV" | "Conda"): string {
  switch (error.error_kind) {
    case "timeout":
      return "Retrying automatically";
    case "import_error":
      return `Check package compatibility in ${envType.toLowerCase()} settings`;
    case "setup_failed":
      return "Retrying automatically";
    default:
      return `Check package name in ${envType.toLowerCase()} settings`;
  }
}

function showSettingsButton(error: PoolErrorWithTimestamp): boolean {
  return (
    error.error_kind === "invalid_package" ||
    error.error_kind === "import_error" ||
    error.error_kind === undefined
  );
}

function PoolErrorItem({ envType, error, onDismiss }: PoolErrorItemProps) {
  const openSettings = () => {
    invoke("open_settings_window").catch((e) => {
      console.error("Failed to open settings:", e);
    });
  };

  const isTimeout = error.error_kind === "timeout";

  return (
    <div className="flex items-center justify-between gap-2 bg-amber-600/90 px-3 py-1.5 text-xs text-white">
      <div className="flex items-center gap-2 min-w-0">
        {isTimeout ? (
          <Clock className="h-3 w-3 flex-shrink-0" />
        ) : (
          <AlertTriangle className="h-3 w-3 flex-shrink-0" />
        )}
        <span className="font-medium flex-shrink-0">{error.message}</span>
        {error.failed_package && (
          <>
            <span className="text-amber-200 flex-shrink-0">—</span>
            <code className="bg-amber-700/50 px-1 rounded text-amber-100 flex-shrink-0">
              {error.failed_package}
            </code>
          </>
        )}
        <span className="text-amber-200 flex-shrink-0">—</span>
        <span className="text-amber-100">{errorSubtitle(error, envType)}</span>
      </div>
      <div className="flex items-center gap-2 flex-shrink-0">
        {showSettingsButton(error) && (
          <button
            type="button"
            onClick={openSettings}
            className="flex items-center gap-1 rounded bg-amber-700/60 px-2 py-0.5 hover:bg-amber-700 transition-colors"
          >
            <Settings className="h-3 w-3" />
            <span>Settings</span>
          </button>
        )}
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
 * with contextual messages based on error type.
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
      {uvError && <PoolErrorItem envType="UV" error={uvError} onDismiss={onDismissUv} />}
      {condaError && (
        <PoolErrorItem envType="Conda" error={condaError} onDismiss={onDismissConda} />
      )}
    </div>
  );
}
