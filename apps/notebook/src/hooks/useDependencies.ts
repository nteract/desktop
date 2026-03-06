import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";

export interface NotebookDependencies {
  dependencies: string[];
  requires_python: string | null;
}

/** Environment sync state from backend */
export type EnvSyncState =
  | { status: "not_running" }
  | { status: "not_uv_managed" }
  | { status: "synced" }
  | { status: "dirty"; added: string[]; removed: string[] };

/** Full pyproject.toml dependencies for display */
export interface PyProjectDeps {
  path: string;
  relative_path: string;
  project_name: string | null;
  dependencies: string[];
  dev_dependencies: string[];
  requires_python: string | null;
  index_url: string | null;
}

/** Info about a detected pyproject.toml */
export interface PyProjectInfo {
  path: string;
  relative_path: string;
  project_name: string | null;
  has_dependencies: boolean;
  dependency_count: number;
  has_dev_dependencies: boolean;
  requires_python: string | null;
  has_venv: boolean;
}

export function useDependencies() {
  const [dependencies, setDependencies] = useState<NotebookDependencies | null>(
    null,
  );
  const [uvAvailable, setUvAvailable] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(false);
  // Track if deps were synced to a running kernel (user may need to restart for some changes)
  const [syncedWhileRunning, setSyncedWhileRunning] = useState(false);
  // Track if user added deps but kernel isn't uv-managed (needs restart)
  const [needsKernelRestart, setNeedsKernelRestart] = useState(false);
  // Environment sync state (dirty detection)
  const [syncState, setSyncState] = useState<EnvSyncState | null>(null);

  // pyproject.toml state
  const [pyprojectInfo, setPyprojectInfo] = useState<PyProjectInfo | null>(
    null,
  );
  const [pyprojectDeps, setPyprojectDeps] = useState<PyProjectDeps | null>(
    null,
  );

  // Check sync state between declared deps and running kernel
  // NOTE: Hot-sync functionality was removed with local kernel mode.
  // In daemon mode, the kernel restarts with new deps. Sync state is always null.
  const checkSyncState = useCallback(async () => {
    // Sync state not available in daemon mode - always null
    setSyncState(null);
  }, []);

  // Check if uv is available and detect pyproject on mount
  useEffect(() => {
    invoke<boolean>("check_uv_available").then(setUvAvailable);
    invoke<PyProjectInfo | null>("detect_pyproject").then(setPyprojectInfo);
  }, []);

  const loadDependencies = useCallback(async () => {
    try {
      const deps = await invoke<NotebookDependencies | null>(
        "get_notebook_dependencies",
      );
      setDependencies(deps);
    } catch (e) {
      logger.error("Failed to load dependencies:", e);
    }
  }, []);

  // Load dependencies on mount
  useEffect(() => {
    loadDependencies();
  }, [loadDependencies]);

  // Re-load when metadata is synced from another window
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlisten = webview.listen("notebook:metadata_updated", () => {
      loadDependencies();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [loadDependencies]);

  // Re-sign the notebook after user modifications to keep it trusted
  const resignTrust = useCallback(async () => {
    try {
      await invoke("approve_notebook_trust");
    } catch (e) {
      // Signing may fail if no trust key yet - that's okay
      logger.debug("[deps] Could not resign trust:", e);
    }
  }, []);

  // Try to sync deps to running kernel
  // NOTE: Hot-sync to a running kernel was removed with local kernel mode.
  // In daemon mode, users need to restart the kernel to pick up new deps.
  const syncToKernel = useCallback(async (): Promise<boolean> => {
    // Hot-sync not available in daemon mode - kernel restart required
    logger.info(
      "[deps] Hot-sync not available in daemon mode, restart kernel to apply changes",
    );
    setNeedsKernelRestart(true);
    return false;
  }, []);

  // Explicit sync function for "Sync Now" button
  const syncNow = useCallback(async (): Promise<boolean> => {
    setLoading(true);
    try {
      const synced = await syncToKernel();
      if (synced) {
        // Refresh sync state after successful sync
        await checkSyncState();
      }
      return synced;
    } finally {
      setLoading(false);
    }
  }, [syncToKernel, checkSyncState]);

  const addDependency = useCallback(
    async (pkg: string) => {
      if (!pkg.trim()) return;
      setLoading(true);
      try {
        await invoke("add_dependency", { package: pkg.trim() });
        await loadDependencies();
        // Re-sign to keep notebook trusted after user modification
        await resignTrust();
        // Check sync state - UI will show "Sync Now" if dirty
        await checkSyncState();
      } catch (e) {
        logger.error("Failed to add dependency:", e);
      } finally {
        setLoading(false);
      }
    },
    [loadDependencies, resignTrust, checkSyncState],
  );

  const removeDependency = useCallback(
    async (pkg: string) => {
      setLoading(true);
      try {
        await invoke("remove_dependency", { package: pkg });
        await loadDependencies();
        // Re-sign to keep notebook trusted after user modification
        await resignTrust();
        // Check sync state - UI will show dirty state
        // Note: removing a dep doesn't uninstall from running kernel
        await checkSyncState();
      } catch (e) {
        logger.error("Failed to remove dependency:", e);
      } finally {
        setLoading(false);
      }
    },
    [loadDependencies, resignTrust, checkSyncState],
  );

  // Remove the entire uv dependency section from notebook metadata
  const clearAllDependencies = useCallback(async () => {
    setLoading(true);
    try {
      await invoke("clear_dependency_section", { section: "uv" });
      await loadDependencies();
      await resignTrust();
    } catch (e) {
      logger.error("Failed to clear UV dependencies:", e);
    } finally {
      setLoading(false);
    }
  }, [loadDependencies, resignTrust]);

  // Clear the synced notice (e.g., when kernel restarts)
  const clearSyncNotice = useCallback(() => {
    setSyncedWhileRunning(false);
    setNeedsKernelRestart(false);
  }, []);

  const setRequiresPython = useCallback(
    async (version: string | null) => {
      setLoading(true);
      try {
        await invoke("set_notebook_dependencies", {
          dependencies: dependencies?.dependencies ?? [],
          requiresPython: version,
        });
        await loadDependencies();
        // Re-sign to keep notebook trusted after user modification
        await resignTrust();
      } catch (e) {
        logger.error("Failed to set requires-python:", e);
      } finally {
        setLoading(false);
      }
    },
    [dependencies, loadDependencies, resignTrust],
  );

  const hasDependencies =
    dependencies !== null && dependencies.dependencies.length > 0;

  // True if uv metadata exists (even with empty deps)
  const isUvConfigured = dependencies !== null;

  // Load full pyproject dependencies
  const loadPyprojectDeps = useCallback(async () => {
    try {
      const deps = await invoke<PyProjectDeps | null>(
        "get_pyproject_dependencies",
      );
      setPyprojectDeps(deps);
    } catch (e) {
      logger.error("Failed to load pyproject dependencies:", e);
    }
  }, []);

  // Load pyproject deps when we detect a pyproject.toml
  useEffect(() => {
    if (pyprojectInfo?.has_dependencies) {
      loadPyprojectDeps();
    }
  }, [pyprojectInfo, loadPyprojectDeps]);

  // Import dependencies from pyproject.toml into notebook metadata
  const importFromPyproject = useCallback(async () => {
    setLoading(true);
    try {
      await invoke("import_pyproject_dependencies");
      await loadDependencies();
      // Re-sign to keep notebook trusted after user modification
      await resignTrust();
      logger.info("[deps] Imported dependencies from pyproject.toml");
    } catch (e) {
      logger.error("Failed to import from pyproject.toml:", e);
    } finally {
      setLoading(false);
    }
  }, [loadDependencies, resignTrust]);

  // Refresh pyproject detection
  const refreshPyproject = useCallback(async () => {
    const info = await invoke<PyProjectInfo | null>("detect_pyproject");
    setPyprojectInfo(info);
    if (info?.has_dependencies) {
      await loadPyprojectDeps();
    } else {
      setPyprojectDeps(null);
    }
  }, [loadPyprojectDeps]);

  return {
    dependencies,
    uvAvailable,
    hasDependencies,
    isUvConfigured,
    loading,
    syncedWhileRunning,
    needsKernelRestart,
    loadDependencies,
    addDependency,
    removeDependency,
    clearAllDependencies,
    setRequiresPython,
    clearSyncNotice,
    // Environment sync state
    syncState,
    syncNow,
    checkSyncState,
    // pyproject.toml support
    pyprojectInfo,
    pyprojectDeps,
    importFromPyproject,
    refreshPyproject,
  };
}
