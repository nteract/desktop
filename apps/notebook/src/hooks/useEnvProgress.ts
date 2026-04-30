import { useCallback, useEffect, useMemo, useState } from "react";
import { EMPTY_ENV_PROGRESS, envProgressKey, projectEnvProgress } from "runtimed";
import { useRuntimeState } from "../lib/runtime-state";

export function useEnvProgress() {
  const runtimeState = useRuntimeState();
  const progressEvent = runtimeState.env.progress;
  const currentKey = envProgressKey(progressEvent);
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
