import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";
import { logger } from "../lib/logger";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "installing-daemon"
  | "error";

interface UpdaterState {
  status: UpdateStatus;
  version: string | null;
  error: string | null;
}

const CHECK_INTERVAL_MS = 30 * 60 * 1000; // 30 minutes
const DAEMON_INSTALL_TIMEOUT_MS = 30 * 1000; // 30 seconds

/** Run a promise with a timeout. Rejects with "timeout" if exceeded. */
function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return Promise.race([
    promise,
    new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error("timeout")), ms),
    ),
  ]);
}

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
      // Install the new daemon BEFORE relaunch to prevent version mismatch.
      // This ensures the new app launches with a compatible daemon already running,
      // avoiding the "restart twice" problem.
      setState((prev) => ({ ...prev, status: "installing-daemon" }));
      try {
        await withTimeout(
          invoke("install_daemon_for_update"),
          DAEMON_INSTALL_TIMEOUT_MS,
        );
        logger.info("[updater] daemon installed, proceeding with relaunch");
      } catch (e) {
        // Log but don't block - worst case, app will upgrade daemon on next launch
        logger.warn("[updater] pre-restart daemon install failed:", e);
      }

      await relaunch();
    } catch (e) {
      logger.error("[updater] relaunch failed:", e);
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
