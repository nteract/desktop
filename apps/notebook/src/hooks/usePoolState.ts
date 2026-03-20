import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { PoolStateEvent } from "../types";

/** Pool error info with timestamp for countdown calculation. */
export interface PoolErrorWithTimestamp {
  /** Error message from the pool. */
  message: string;
  /** When this state was received (epoch ms). */
  receivedAt: number;
}

/** Full pool state including counts and errors. */
export interface PoolState {
  uvAvailable: number;
  uvWarming: number;
  uvPoolSize: number;
  uvError: PoolErrorWithTimestamp | null;
  condaAvailable: number;
  condaWarming: number;
  condaPoolSize: number;
  condaError: PoolErrorWithTimestamp | null;
}

const INITIAL_STATE: PoolState = {
  uvAvailable: 0,
  uvWarming: 0,
  uvPoolSize: 0,
  uvError: null,
  condaAvailable: 0,
  condaWarming: 0,
  condaPoolSize: 0,
  condaError: null,
};

function errorsEqual(
  a: PoolErrorWithTimestamp | null,
  b: PoolErrorWithTimestamp | null,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.message === b.message;
}

/**
 * Subscribe to daemon pool state via the PoolDoc Automerge sync.
 *
 * Returns pool counts (available/warming/pool_size) and error info for
 * both UV and Conda pools. Errors include dismiss support — dismissed
 * errors reappear when the error message changes.
 */
export function usePoolState() {
  const [state, setState] = useState<PoolState>(INITIAL_STATE);

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

      const uvError: PoolErrorWithTimestamp | null = payload.uv_error
        ? { message: payload.uv_error, receivedAt: now }
        : null;
      const condaError: PoolErrorWithTimestamp | null = payload.conda_error
        ? { message: payload.conda_error, receivedAt: now }
        : null;

      // Check if errors changed — reset dismiss state on new errors
      const prev = prevStateRef.current;
      if (!errorsEqual(uvError, prev.uvError)) {
        setDismissedUv(false);
      }
      if (!errorsEqual(condaError, prev.condaError)) {
        setDismissedConda(false);
      }

      // Update state and ref
      const newState: PoolState = {
        uvAvailable: payload.uv_available,
        uvWarming: payload.uv_warming,
        uvPoolSize: payload.uv_pool_size,
        uvError,
        condaAvailable: payload.conda_available,
        condaWarming: payload.conda_warming,
        condaPoolSize: payload.conda_pool_size,
        condaError,
      };
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
    // Pool counts
    uvAvailable: state.uvAvailable,
    uvWarming: state.uvWarming,
    uvPoolSize: state.uvPoolSize,
    condaAvailable: state.condaAvailable,
    condaWarming: state.condaWarming,
    condaPoolSize: state.condaPoolSize,

    // Errors (null when dismissed or no error)
    uvError: dismissedUv ? null : state.uvError,
    condaError: dismissedConda ? null : state.condaError,
    hasErrors:
      (!dismissedUv && state.uvError !== null) ||
      (!dismissedConda && state.condaError !== null),

    // Dismiss controls
    dismissUvError,
    dismissCondaError,
    dismissAll,
  };
}
