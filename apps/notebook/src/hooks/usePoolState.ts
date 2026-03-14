import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type { PoolError, PoolStateEvent } from "../types";

/** PoolError with timestamp for countdown calculation */
export interface PoolErrorWithTimestamp extends PoolError {
  /** When this state was received (epoch ms) */
  receivedAt: number;
}

export interface PoolState {
  uvError: PoolErrorWithTimestamp | null;
  condaError: PoolErrorWithTimestamp | null;
}

/**
 * Hook that listens to pool state broadcasts from the daemon.
 *
 * Reports prewarm pool errors (e.g., typo'd package in default_packages)
 * so the UI can display warnings with retry countdowns.
 */
export function usePoolState() {
  const [state, setState] = useState<PoolState>({
    uvError: null,
    condaError: null,
  });

  // Track dismissed errors so they don't reappear until state changes
  const [dismissedUv, setDismissedUv] = useState(false);
  const [dismissedConda, setDismissedConda] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const unlisten = listen<PoolStateEvent>("pool:state", (event) => {
      if (cancelled) return;

      const now = Date.now();
      const payload = event.payload;

      setState((prev) => {
        const uvError = payload.uv_error
          ? { ...payload.uv_error, receivedAt: now }
          : null;
        const condaError = payload.conda_error
          ? { ...payload.conda_error, receivedAt: now }
          : null;

        // Reset dismissed state if error changed (new error or cleared)
        if (uvError?.message !== prev.uvError?.message) {
          setDismissedUv(false);
        }
        if (condaError?.message !== prev.condaError?.message) {
          setDismissedConda(false);
        }

        return { uvError, condaError };
      });
    });

    return () => {
      cancelled = true;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const dismissUvError = useCallback(() => {
    setDismissedUv(true);
  }, []);

  const dismissCondaError = useCallback(() => {
    setDismissedConda(true);
  }, []);

  const dismissAll = useCallback(() => {
    setDismissedUv(true);
    setDismissedConda(true);
  }, []);

  return {
    uvError: dismissedUv ? null : state.uvError,
    condaError: dismissedConda ? null : state.condaError,
    hasErrors:
      (!dismissedUv && state.uvError !== null) ||
      (!dismissedConda && state.condaError !== null),
    dismissUvError,
    dismissCondaError,
    dismissAll,
  };
}
