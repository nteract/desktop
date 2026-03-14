import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
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

/** Compare two pool errors for equality (all fields except receivedAt) */
function errorsEqual(
  a: PoolError | null | undefined,
  b: PoolError | null | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.message === b.message &&
    a.failed_package === b.failed_package &&
    a.consecutive_failures === b.consecutive_failures &&
    a.retry_in_secs === b.retry_in_secs
  );
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

  // Track previous errors to detect changes outside of setState
  const prevStateRef = useRef<PoolState>(state);

  useEffect(() => {
    let cancelled = false;

    const unlisten = listen<PoolStateEvent>("pool:state", (event) => {
      if (cancelled) return;

      const now = Date.now();
      const payload = event.payload;

      const uvError = payload.uv_error
        ? { ...payload.uv_error, receivedAt: now }
        : null;
      const condaError = payload.conda_error
        ? { ...payload.conda_error, receivedAt: now }
        : null;

      // Check if errors changed (compare all fields, not just message)
      const prev = prevStateRef.current;
      if (!errorsEqual(uvError, prev.uvError)) {
        setDismissedUv(false);
      }
      if (!errorsEqual(condaError, prev.condaError)) {
        setDismissedConda(false);
      }

      // Update state and ref
      const newState = { uvError, condaError };
      prevStateRef.current = newState;
      setState(newState);
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
