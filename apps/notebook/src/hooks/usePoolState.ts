import { useCallback, useRef, useState } from "react";
import {
  type PoolErrorWithTimestamp,
  type PoolState,
  usePoolState as usePoolStateStore,
} from "../lib/pool-state";

export type { PoolErrorWithTimestamp } from "../lib/pool-state";

/** Compare two pool errors for equality (all fields except receivedAt) */
function errorsEqual(
  a: PoolErrorWithTimestamp | null | undefined,
  b: PoolErrorWithTimestamp | null | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.message === b.message &&
    a.failed_package === b.failed_package &&
    a.error_kind === b.error_kind &&
    a.consecutive_failures === b.consecutive_failures &&
    a.retry_in_secs === b.retry_in_secs
  );
}

/** Extract error info from a RuntimePoolState, or null if healthy. */
function extractError(pool: PoolState["uv"] | PoolState["conda"]): PoolErrorWithTimestamp | null {
  if (!pool.error) return null;
  return {
    message: pool.error,
    failed_package: pool.failed_package,
    error_kind: pool.error_kind,
    consecutive_failures: pool.consecutive_failures,
    retry_in_secs: pool.retry_in_secs,
    receivedAt: Date.now(),
  };
}

/**
 * Hook that reads pool state from the daemon's PoolDoc (Automerge sync).
 *
 * Reports prewarm pool errors (e.g., typo'd package in default_packages)
 * so the UI can display warnings with retry countdowns.
 */
export function usePoolState() {
  const poolState = usePoolStateStore();

  // Track dismissed errors so they don't reappear until state changes
  const [dismissedUv, setDismissedUv] = useState(false);
  const [dismissedConda, setDismissedConda] = useState(false);

  // Track previous errors to detect changes and reset dismiss state
  const prevUvErrorRef = useRef<PoolErrorWithTimestamp | null>(null);
  const prevCondaErrorRef = useRef<PoolErrorWithTimestamp | null>(null);

  const uvError = extractError(poolState.uv);
  const condaError = extractError(poolState.conda);

  // Reset dismiss state when error changes
  if (!errorsEqual(uvError, prevUvErrorRef.current)) {
    prevUvErrorRef.current = uvError;
    if (dismissedUv) setDismissedUv(false);
  }
  if (!errorsEqual(condaError, prevCondaErrorRef.current)) {
    prevCondaErrorRef.current = condaError;
    if (dismissedConda) setDismissedConda(false);
  }

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
    uvError: dismissedUv ? null : uvError,
    condaError: dismissedConda ? null : condaError,
    hasErrors: (!dismissedUv && uvError !== null) || (!dismissedConda && condaError !== null),
    dismissUvError,
    dismissCondaError,
    dismissAll,
  };
}
