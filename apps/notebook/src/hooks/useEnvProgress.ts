import { useCallback, useEffect, useMemo, useState } from "react";
import type { EnvProgressEnvType, EnvProgressEvent, EnvProgressPhase } from "runtimed";
import { useRuntimeState } from "../lib/runtime-state";

export interface EnvProgressState {
  /** Whether environment preparation is currently active */
  isActive: boolean;
  /** Current phase name */
  phase: string | null;
  /** Environment type (conda, uv, pixi, or a future daemon-provided manager). */
  envType: EnvProgressEnvType | null;
  /** Error message if phase is "error" */
  error: string | null;
  /** Human-readable status text */
  statusText: string;
  /** Elapsed time for current/last operation in ms */
  elapsedMs: number | null;
  /** Progress tracking for download/install phases */
  progress: { completed: number; total: number } | null;
  /** Download speed in bytes per second */
  bytesPerSecond: number | null;
  /** Current package being processed */
  currentPackage: string | null;
}

/** Format bytes as human-readable string */
function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }
  return `${bytes} B`;
}

export function getStatusText(event: EnvProgressEvent): string {
  const phase = event.phase;
  switch (phase) {
    case "starting":
      return "Preparing environment...";
    case "cache_hit":
      return "Using cached environment";
    case "lock_file_hit":
      return "Rebuilding from lock file";
    case "offline_hit":
      return "Using cached packages";
    case "fetching_repodata": {
      const e = event as Extract<EnvProgressPhase, { phase: "fetching_repodata" }>;
      return `Fetching package index (${e.channels.join(", ")})`;
    }
    case "repodata_complete": {
      const e = event as Extract<EnvProgressPhase, { phase: "repodata_complete" }>;
      return `Loaded ${e.record_count.toLocaleString()} packages`;
    }
    case "solving": {
      const e = event as Extract<EnvProgressPhase, { phase: "solving" }>;
      return `Solving dependencies (${e.spec_count} specs)`;
    }
    case "solve_complete": {
      const e = event as Extract<EnvProgressPhase, { phase: "solve_complete" }>;
      return `Resolved ${e.package_count} packages`;
    }
    case "installing": {
      const e = event as Extract<EnvProgressPhase, { phase: "installing" }>;
      return `Installing ${e.total} packages...`;
    }
    case "download_progress": {
      const e = event as Extract<EnvProgressPhase, { phase: "download_progress" }>;
      const speed = `${formatBytes(e.bytes_per_second)}/s`;
      if (e.current_package) {
        return `Downloading ${e.completed}/${e.total} ${e.current_package} @ ${speed}`;
      }
      return `Downloading ${e.completed}/${e.total} @ ${speed}`;
    }
    case "link_progress": {
      const e = event as Extract<EnvProgressPhase, { phase: "link_progress" }>;
      if (e.current_package) {
        return `Installing ${e.completed}/${e.total} ${e.current_package}`;
      }
      return `Installing ${e.completed}/${e.total}`;
    }
    case "install_complete":
      return "Installation complete";
    case "creating_venv":
      return "Creating virtual environment...";
    case "installing_packages": {
      const e = event as Extract<EnvProgressPhase, { phase: "installing_packages" }>;
      return `Installing ${e.packages.length} packages...`;
    }
    case "ready":
      return "Environment ready";
    case "error":
      return "Environment error";
    default:
      return "Preparing...";
  }
}

const EMPTY_ENV_PROGRESS: EnvProgressState = {
  isActive: false,
  phase: null,
  envType: null,
  error: null,
  statusText: "",
  elapsedMs: null,
  progress: null,
  bytesPerSecond: null,
  currentPackage: null,
};

function extractProgress(event: EnvProgressEvent): { completed: number; total: number } | null {
  const phase = event.phase;
  if (phase === "download_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "download_progress" }>;
    return { completed: e.completed, total: e.total };
  }
  if (phase === "link_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "link_progress" }>;
    return { completed: e.completed, total: e.total };
  }
  return null;
}

export function projectEnvProgress(event: EnvProgressEvent | null): EnvProgressState {
  if (!event) return EMPTY_ENV_PROGRESS;

  const phase = event.phase;
  // Only terminal success states stop active progress. `error` is terminal
  // too, but remains visible until the caller locally resets/dismisses it.
  const isTerminalSuccess = phase === "ready" || phase === "cache_hit" || phase === "offline_hit";
  const isError = phase === "error";
  const error = isError ? (event as Extract<EnvProgressPhase, { phase: "error" }>).message : null;

  let elapsedMs: number | null = null;
  if ("elapsed_ms" in event && typeof event.elapsed_ms === "number") {
    elapsedMs = event.elapsed_ms;
  }

  let bytesPerSecond: number | null = null;
  if (phase === "download_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "download_progress" }>;
    bytesPerSecond = e.bytes_per_second;
  }

  let currentPackage: string | null = null;
  if (phase === "download_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "download_progress" }>;
    currentPackage = e.current_package || null;
  } else if (phase === "link_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "link_progress" }>;
    currentPackage = e.current_package || null;
  }

  return {
    isActive: !isTerminalSuccess && !isError,
    phase,
    envType: event.env_type,
    error,
    statusText: getStatusText(event),
    elapsedMs,
    progress: extractProgress(event),
    bytesPerSecond,
    currentPackage,
  };
}

function progressKey(event: EnvProgressEvent | null): string | null {
  return event ? JSON.stringify(event) : null;
}

export function useEnvProgress() {
  const runtimeState = useRuntimeState();
  const progressEvent = runtimeState.env.progress;
  const currentKey = progressKey(progressEvent);
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);

  useEffect(() => {
    if (currentKey !== dismissedKey && dismissedKey !== null) {
      setDismissedKey(null);
    }
  }, [currentKey, dismissedKey]);

  const state = useMemo(() => {
    if (currentKey && currentKey === dismissedKey) {
      return EMPTY_ENV_PROGRESS;
    }
    return projectEnvProgress(progressEvent);
  }, [currentKey, dismissedKey, progressEvent]);

  const reset = useCallback(() => {
    setDismissedKey(currentKey);
  }, [currentKey]);

  return { ...state, reset };
}
