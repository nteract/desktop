import { invoke } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";
import { logger } from "../lib/logger";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "error";

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
  const updateRef = useRef<Update | null>(null);

  const checkForUpdate = useCallback(async () => {
    try {
      setState((prev) => ({ ...prev, status: "checking", error: null }));
      const update = await check();
      if (update) {
        updateRef.current = update;
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

  const downloadAndInstall = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;

    try {
      setState((prev) => ({ ...prev, status: "downloading" }));
      await update.downloadAndInstall();
      setState((prev) => ({ ...prev, status: "ready" }));
    } catch (e) {
      logger.error("[updater] download/install failed:", e);
      setState((prev) => ({
        ...prev,
        status: "error",
        error: String(e),
      }));
    }
  }, []);

  const restartToUpdate = useCallback(async () => {
    try {
      // Open the dedicated upgrade screen, which handles:
      // - Showing open notebooks with kernel status
      // - Letting user abort busy kernels
      // - Saving dirty notebooks
      // - Shutting down kernels gracefully
      // - Installing the new daemon
      // - Restarting the app
      logger.info("[updater] opening upgrade screen");
      await invoke("begin_upgrade");
      // Upgrade screen takes over from here - this window will be closed
    } catch (e) {
      logger.error("[updater] failed to open upgrade screen:", e);
      // Revert to ready state so user can retry
      setState((prev) => ({ ...prev, status: "ready" }));
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
    downloadAndInstall,
    restartToUpdate,
  };
}
