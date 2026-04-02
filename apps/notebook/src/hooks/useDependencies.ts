import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";
import {
  addUvDependency,
  clearUvSection,
  removeUvDependency,
  setUvPrerelease,
  setUvRequiresPython,
  useUvDependencies,
} from "../lib/notebook-metadata";

export interface NotebookDependencies {
  dependencies: string[];
  requires_python: string | null;
  prerelease: string | null;
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
  const [loading, setLoading] = useState(false);

  // pyproject.toml state
  const [pyprojectInfo, setPyprojectInfo] = useState<PyProjectInfo | null>(
    null,
  );
  const [pyprojectDeps, setPyprojectDeps] = useState<PyProjectDeps | null>(
    null,
  );

  // Detect pyproject on mount
  useEffect(() => {
    invoke<PyProjectInfo | null>("detect_pyproject").then(setPyprojectInfo);
  }, []);

  // Reactive read from the WASM Automerge doc via useSyncExternalStore.
  // Re-renders automatically when the doc changes (bootstrap, sync, writes).
  const uvDeps = useUvDependencies();
  const dependencies = uvDeps
    ? {
        dependencies: uvDeps.dependencies,
        requires_python: uvDeps.requiresPython,
        prerelease: uvDeps.prerelease,
      }
    : null;

  // Re-sign the notebook after user modifications to keep it trusted
  const resignTrust = useCallback(async () => {
    try {
      await invoke("approve_notebook_trust");
    } catch (e) {
      // Signing may fail if no trust key yet - that's okay
      logger.debug("[deps] Could not resign trust:", e);
    }
  }, []);

  const addDependency = useCallback(
    async (pkg: string) => {
      if (!pkg.trim()) return;
      setLoading(true);
      try {
        await addUvDependency(pkg.trim());
        await resignTrust();
      } catch (e) {
        logger.error("Failed to add dependency:", e);
      } finally {
        setLoading(false);
      }
    },
    [resignTrust],
  );

  const removeDependency = useCallback(
    async (pkg: string) => {
      setLoading(true);
      try {
        await removeUvDependency(pkg);
        await resignTrust();
      } catch (e) {
        logger.error("Failed to remove dependency:", e);
      } finally {
        setLoading(false);
      }
    },
    [resignTrust],
  );

  // Remove the entire uv dependency section from notebook metadata
  const clearAllDependencies = useCallback(async () => {
    setLoading(true);
    try {
      await clearUvSection();
      await resignTrust();
    } catch (e) {
      logger.error("Failed to clear UV dependencies:", e);
    } finally {
      setLoading(false);
    }
  }, [resignTrust]);

  const setRequiresPython = useCallback(
    async (version: string | null) => {
      setLoading(true);
      try {
        await setUvRequiresPython(version);
        await resignTrust();
      } catch (e) {
        logger.error("Failed to set requires-python:", e);
      } finally {
        setLoading(false);
      }
    },
    [resignTrust],
  );

  const setPrerelease = useCallback(
    async (prerelease: string | null) => {
      setLoading(true);
      try {
        await setUvPrerelease(prerelease);
        await resignTrust();
      } catch (e) {
        logger.error("Failed to set prerelease:", e);
      } finally {
        setLoading(false);
      }
    },
    [resignTrust],
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
      // Re-sign to keep notebook trusted after user modification
      await resignTrust();
      logger.info("[deps] Imported dependencies from pyproject.toml");
    } catch (e) {
      logger.error("Failed to import from pyproject.toml:", e);
    } finally {
      setLoading(false);
    }
  }, [resignTrust]);

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
    hasDependencies,
    isUvConfigured,
    loading,

    addDependency,
    removeDependency,
    clearAllDependencies,
    setRequiresPython,
    setPrerelease,
    // pyproject.toml support
    pyprojectInfo,
    pyprojectDeps,
    importFromPyproject,
    refreshPyproject,
  };
}
