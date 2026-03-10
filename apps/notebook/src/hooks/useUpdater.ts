import { invoke } from "@tauri-apps/api/core";
import { check } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";

export type UpdateStatus = "idle" | "checking" | "available" | "error";

interface UpdaterState {
  status: UpdateStatus;
  version: string | null;
  error: string | null;
}

const CHECK_INTERVAL_MS = 30 * 60 * 1000; // 30 minutes

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({
    status: "idle",
    version: null,
    error: null,
  });

  const checkForUpdate = useCallback(async () => {
    try {
      setState((prev) => ({ ...prev, status: "checking", error: null }));
      const update = await check();
      if (update) {
        setState({
          status: "available",
          version: update.version,
          error: null,
        });
      } else {
        setState({ status: "idle", version: null, error: null });
      }
    } catch (e) {
      logger.warn("[updater] check failed:", e);
      setState((prev) => ({
        ...prev,
        status: "error",
        error: String(e),
      }));
    }
  }, []);

  const restartToUpdate = useCallback(async () => {
    try {
      logger.info("[updater] opening upgrade screen");
      await invoke("begin_upgrade");
    } catch (e) {
      logger.error("[updater] failed to open upgrade screen:", e);
      setState((prev) => ({ ...prev, status: "available" }));
    }
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => checkForUpdate(), 5000);
    const interval = setInterval(checkForUpdate, CHECK_INTERVAL_MS);
    return () => {
      clearTimeout(timer);
      clearInterval(interval);
    };
  }, [checkForUpdate]);

  return {
    ...state,
    checkForUpdate,
    restartToUpdate,
  };
}
