import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type {
  DaemonBroadcast,
  EnvProgressEvent,
  EnvProgressPhase,
} from "../types";

export interface EnvProgressState {
  /** Whether environment preparation is currently active */
  isActive: boolean;
  /** Current phase name */
  phase: string | null;
  /** Environment type (conda or uv) */
  envType: "conda" | "uv" | null;
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

function getStatusText(event: EnvProgressEvent): string {
  const phase = event.phase;
  switch (phase) {
    case "starting":
      return "Preparing environment...";
    case "cache_hit":
      return "Using cached environment";
    case "fetching_repodata": {
      const e = event as Extract<
        EnvProgressPhase,
        { phase: "fetching_repodata" }
      >;
      return `Fetching package index (${e.channels.join(", ")})`;
    }
    case "repodata_complete": {
      const e = event as Extract<
        EnvProgressPhase,
        { phase: "repodata_complete" }
      >;
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
      const e = event as Extract<
        EnvProgressPhase,
        { phase: "download_progress" }
      >;
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
      const e = event as Extract<
        EnvProgressPhase,
        { phase: "installing_packages" }
      >;
      return `Installing ${e.packages.length} packages...`;
    }
    case "ready":
      return "Environment ready";
    case "error": {
      const e = event as Extract<EnvProgressPhase, { phase: "error" }>;
      return `Error: ${e.message}`;
    }
    default:
      return "Preparing...";
  }
}

function extractProgress(
  event: EnvProgressEvent,
): { completed: number; total: number } | null {
  const phase = event.phase;
  if (phase === "download_progress") {
    const e = event as Extract<
      EnvProgressPhase,
      { phase: "download_progress" }
    >;
    return { completed: e.completed, total: e.total };
  }
  if (phase === "link_progress") {
    const e = event as Extract<EnvProgressPhase, { phase: "link_progress" }>;
    return { completed: e.completed, total: e.total };
  }
  return null;
}

export function useEnvProgress() {
  const [state, setState] = useState<EnvProgressState>({
    isActive: false,
    phase: null,
    envType: null,
    error: null,
    statusText: "",
    elapsedMs: null,
    progress: null,
    bytesPerSecond: null,
    currentPackage: null,
  });

  useEffect(() => {
    let cancelled = false;

    const processEvent = (payload: EnvProgressEvent) => {
      if (cancelled) return;

      const phase = payload.phase;
      // Only "ready" and "cache_hit" are terminal success states
      // "error" is terminal but we keep error visible
      const isTerminalSuccess = phase === "ready" || phase === "cache_hit";
      const isError = phase === "error";
      const error = isError
        ? (payload as Extract<EnvProgressPhase, { phase: "error" }>).message
        : null;

      // Extract elapsed_ms from phases that have it
      let elapsedMs: number | null = null;
      if ("elapsed_ms" in payload && typeof payload.elapsed_ms === "number") {
        elapsedMs = payload.elapsed_ms;
      }

      // Extract progress from download/link phases
      const progress = extractProgress(payload);

      // Extract bytes per second from download phase
      let bytesPerSecond: number | null = null;
      if (phase === "download_progress") {
        const e = payload as Extract<
          EnvProgressPhase,
          { phase: "download_progress" }
        >;
        bytesPerSecond = e.bytes_per_second;
      }

      // Extract current package
      let currentPackage: string | null = null;
      if (phase === "download_progress") {
        const e = payload as Extract<
          EnvProgressPhase,
          { phase: "download_progress" }
        >;
        currentPackage = e.current_package || null;
      } else if (phase === "link_progress") {
        const e = payload as Extract<
          EnvProgressPhase,
          { phase: "link_progress" }
        >;
        currentPackage = e.current_package || null;
      }

      setState((prev) => ({
        // Keep active until success; errors stay visible until reset
        isActive: !isTerminalSuccess && !isError,
        phase,
        envType: payload.env_type,
        // Keep previous error if we're in error state and don't have a new error
        error: error ?? (isError ? prev.error : null),
        statusText: getStatusText(payload),
        elapsedMs,
        progress,
        bytesPerSecond,
        currentPackage,
      }));
    };

    // Listen for direct env:progress events (from notebook process)
    const unlisten = listen<EnvProgressEvent>("env:progress", (event) => {
      processEvent(event.payload);
    });

    // Also listen for daemon:broadcast events with env_progress
    // (from daemon-managed environment preparation during kernel launch)
    const unlistenBroadcast = listen<DaemonBroadcast>(
      "daemon:broadcast",
      (event) => {
        const broadcast = event.payload;
        if (broadcast.event === "env_progress") {
          // The daemon broadcast has the same shape as EnvProgressEvent
          // (env_type + flattened phase fields) plus the "event" tag
          processEvent(broadcast as unknown as EnvProgressEvent);
        }
      },
    );

    return () => {
      cancelled = true;
      unlisten.then((fn) => fn());
      unlistenBroadcast.then((fn) => fn());
    };
  }, []);

  const reset = useCallback(() => {
    setState({
      isActive: false,
      phase: null,
      envType: null,
      error: null,
      statusText: "",
      elapsedMs: null,
      progress: null,
      bytesPerSecond: null,
      currentPackage: null,
    });
  }, []);

  return { ...state, reset };
}
