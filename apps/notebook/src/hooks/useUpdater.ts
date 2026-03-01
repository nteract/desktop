import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";

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
      console.warn("[updater] check failed:", e);
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
      console.error("[updater] download/install failed:", e);
      setState((prev) => ({
        ...prev,
        status: "error",
        error: String(e),
      }));
    }
  }, []);

  const restartToUpdate = useCallback(async () => {
    try {
      await relaunch();
    } catch (e) {
      console.error("[updater] relaunch failed:", e);
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
