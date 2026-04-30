import { useCallback, useMemo, useState } from "react";
import { derivePyproject } from "runtimed";
import { logger } from "../lib/logger";
import {
  addUvDependency,
  clearUvSection,
  removeUvDependency,
  setUvPrerelease,
  setUvRequiresPython,
  useUvDependencies,
} from "../lib/notebook-metadata";
import { useRuntimeState } from "../lib/runtime-state";

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

export type { PyProjectDeps, PyProjectInfo } from "runtimed";

export function useDependencies() {
  const [loading, setLoading] = useState(false);
  const runtimeState = useRuntimeState();

  // Reactive read from the WASM Automerge doc via useSyncExternalStore.
  // Re-renders automatically when notebook metadata changes.
  const uvDeps = useUvDependencies();
  const dependencies = uvDeps
    ? {
        dependencies: uvDeps.dependencies,
        requires_python: uvDeps.requiresPython,
        prerelease: uvDeps.prerelease,
      }
    : null;

  // Trust re-signing lives on the daemon now (issue #2118). When the WASM
  // dep write arrives via Automerge sync, the daemon keeps a previously
  // Trusted notebook Trusted by auto re-signing. Frontend hooks just
  // write to the CRDT.

  const addDependency = useCallback(async (pkg: string) => {
    if (!pkg.trim()) return;
    setLoading(true);
    try {
      await addUvDependency(pkg.trim());
    } catch (e) {
      logger.error("Failed to add dependency:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  const removeDependency = useCallback(async (pkg: string) => {
    setLoading(true);
    try {
      await removeUvDependency(pkg);
    } catch (e) {
      logger.error("Failed to remove dependency:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  // Remove the entire uv dependency section from notebook metadata
  const clearAllDependencies = useCallback(async () => {
    setLoading(true);
    try {
      await clearUvSection();
    } catch (e) {
      logger.error("Failed to clear UV dependencies:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  const setRequiresPython = useCallback(async (version: string | null) => {
    setLoading(true);
    try {
      await setUvRequiresPython(version);
    } catch (e) {
      logger.error("Failed to set requires-python:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  const setPrerelease = useCallback(async (prerelease: string | null) => {
    setLoading(true);
    try {
      await setUvPrerelease(prerelease);
    } catch (e) {
      logger.error("Failed to set prerelease:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  const hasDependencies = dependencies !== null && dependencies.dependencies.length > 0;

  // True if uv metadata exists (even with empty deps)
  const isUvConfigured = dependencies !== null;

  // Derive pyproject info + deps from RuntimeState.project_context. The
  // daemon writes this field on notebook open and on save-as; clients
  // read it via the normal Automerge sync. See issue #2208.
  const { pyprojectInfo, pyprojectDeps } = useMemo(
    () => derivePyproject(runtimeState.project_context),
    [runtimeState.project_context],
  );

  // Import dependencies from pyproject.toml into notebook metadata.
  // Reads from the synced CRDT snapshot and writes via the existing
  // UV metadata helpers. Deduplication is handled by `addUvDependency`
  // in notebook-doc (case-insensitive), so repeat imports stay safe.
  const importFromPyproject = useCallback(async () => {
    if (!pyprojectDeps) {
      logger.warn("[deps] importFromPyproject called with no pyproject detected");
      return;
    }
    setLoading(true);
    try {
      const all = [...pyprojectDeps.dependencies, ...pyprojectDeps.dev_dependencies];
      for (const pkg of all) {
        await addUvDependency(pkg);
      }
      if (pyprojectDeps.requires_python !== null) {
        await setUvRequiresPython(pyprojectDeps.requires_python);
      }
      logger.info(`[deps] Imported ${all.length} dependencies from pyproject.toml`);
    } catch (e) {
      logger.error("Failed to import from pyproject.toml:", e);
    } finally {
      setLoading(false);
    }
  }, [pyprojectDeps]);

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
  };
}
