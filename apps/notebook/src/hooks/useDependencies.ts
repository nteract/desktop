import { useCallback, useMemo, useState } from "react";
import type { ProjectContext, ProjectFileKind } from "runtimed";
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

/**
 * Full pyproject.toml dependencies for display.
 *
 * Derived from `RuntimeState.project_context`. Fields the daemon does
 * not currently surface (`project_name`, `index_url`) are emitted as
 * `null`; UI code treats them as optional today.
 */
export interface PyProjectDeps {
  path: string;
  relative_path: string;
  project_name: string | null;
  dependencies: string[];
  dev_dependencies: string[];
  requires_python: string | null;
  index_url: string | null;
}

/**
 * Info about a detected pyproject.toml.
 *
 * Derived from `RuntimeState.project_context`. `has_venv` is `false`
 * because the daemon does not currently check the filesystem for a
 * `.venv` next to the project file; consumers must not treat this as
 * authoritative for "is the env built yet."
 */
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

/**
 * Project-file kind the frontend cares about here. Narrowed from the
 * CRDT's `ProjectFileKind` so we only select pyproject detections.
 */
const PYPROJECT: ProjectFileKind = "PyprojectToml";

/**
 * Derive `PyProjectInfo` + `PyProjectDeps` from a `ProjectContext`. Pure;
 * exported for tests. Returns both `null` when the context is not a
 * Detected pyproject.toml.
 */
export function derivePyproject(ctx: ProjectContext): {
  pyprojectInfo: PyProjectInfo | null;
  pyprojectDeps: PyProjectDeps | null;
} {
  if (ctx.state !== "Detected" || ctx.project_file.kind !== PYPROJECT) {
    return { pyprojectInfo: null, pyprojectDeps: null };
  }
  const { project_file, parsed } = ctx;
  const shared = {
    path: project_file.absolute_path,
    relative_path: project_file.relative_to_notebook,
    project_name: null,
    requires_python: parsed.requires_python,
  };
  return {
    pyprojectInfo: {
      ...shared,
      has_dependencies: parsed.dependencies.length > 0,
      dependency_count: parsed.dependencies.length,
      has_dev_dependencies: parsed.dev_dependencies.length > 0,
      has_venv: false,
    },
    pyprojectDeps: {
      ...shared,
      dependencies: parsed.dependencies,
      dev_dependencies: parsed.dev_dependencies,
      index_url: null,
    },
  };
}

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
