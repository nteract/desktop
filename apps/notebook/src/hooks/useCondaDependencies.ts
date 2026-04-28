import { useCallback, useMemo, useState } from "react";
import type { ProjectContext, ProjectFileExtras } from "runtimed";
import { logger } from "../lib/logger";
import {
  addCondaDependency as addCondaDepWasm,
  clearCondaSection,
  removeCondaDependency as removeCondaDepWasm,
  setCondaChannels as setCondaChannelsWasm,
  setCondaPython as setCondaPythonWasm,
  useCondaDeps,
} from "../lib/notebook-metadata";
import { useRuntimeState } from "../lib/runtime-state";

export interface CondaDependencies {
  dependencies: string[];
  channels: string[];
  python: string | null;
}

/**
 * Info about a detected environment.yml.
 *
 * Derived from `RuntimeState.project_context` (see #2208). Some fields
 * that the old app-side walker produced (`name`) are not currently
 * surfaced through `ProjectFileParsed`; they're emitted as `null` and
 * the UI treats them as optional display.
 */
export interface EnvironmentYmlInfo {
  path: string;
  relative_path: string;
  name: string | null;
  has_dependencies: boolean;
  dependency_count: number;
  has_pip_dependencies: boolean;
  pip_dependency_count: number;
  python: string | null;
  channels: string[];
}

/** Full environment.yml dependencies for display. */
export interface EnvironmentYmlDeps {
  path: string;
  relative_path: string;
  name: string | null;
  dependencies: string[];
  pip_dependencies: string[];
  python: string | null;
  channels: string[];
}

/** Conda sync state — tracks whether declared deps match the running kernel's environment. */
export type CondaSyncState =
  | { status: "not_running" }
  | { status: "not_conda_managed" }
  | { status: "synced" }
  | { status: "dirty" };

function envYmlExtras(extras: ProjectFileExtras): { channels: string[]; pip: string[] } {
  if (extras.kind === "EnvironmentYml") {
    return { channels: extras.channels, pip: extras.pip };
  }
  return { channels: [], pip: [] };
}

/**
 * Derive `EnvironmentYmlInfo` + `EnvironmentYmlDeps` from a
 * `ProjectContext`. Pure; exported for tests.
 *
 * Returns both `null` when the context is not a Detected environment.yml.
 */
export function deriveEnvironmentYml(ctx: ProjectContext): {
  environmentYmlInfo: EnvironmentYmlInfo | null;
  environmentYmlDeps: EnvironmentYmlDeps | null;
} {
  if (ctx.state !== "Detected" || ctx.project_file.kind !== "EnvironmentYml") {
    return { environmentYmlInfo: null, environmentYmlDeps: null };
  }
  const { channels, pip } = envYmlExtras(ctx.parsed.extras);
  const shared = {
    path: ctx.project_file.absolute_path,
    relative_path: ctx.project_file.relative_to_notebook,
    name: null,
    python: ctx.parsed.requires_python,
    channels,
  };
  return {
    environmentYmlInfo: {
      ...shared,
      has_dependencies: ctx.parsed.dependencies.length > 0,
      dependency_count: ctx.parsed.dependencies.length,
      has_pip_dependencies: pip.length > 0,
      pip_dependency_count: pip.length,
    },
    environmentYmlDeps: {
      ...shared,
      dependencies: ctx.parsed.dependencies,
      pip_dependencies: pip,
    },
  };
}

export function useCondaDependencies() {
  const [loading, setLoading] = useState(false);
  const runtimeState = useRuntimeState();

  // Reactive read from the WASM Automerge doc via useSyncExternalStore.
  // Re-renders automatically when notebook metadata changes.
  const condaDeps = useCondaDeps();
  const dependencies = condaDeps
    ? {
        dependencies: condaDeps.dependencies,
        channels: condaDeps.channels,
        python: condaDeps.python,
      }
    : null;

  // Derive environment.yml info + deps from RuntimeState.project_context.
  const { environmentYmlInfo, environmentYmlDeps } = useMemo(
    () => deriveEnvironmentYml(runtimeState.project_context),
    [runtimeState.project_context],
  );

  // Trust re-signing lives on the daemon now (issue #2118). The daemon
  // keeps a previously Trusted notebook Trusted by auto re-signing when
  // the WASM dep write arrives via Automerge sync.
  const withLoading = useCallback(async (op: () => Promise<void>, label: string) => {
    setLoading(true);
    try {
      await op();
    } catch (e) {
      logger.error(`Failed to ${label}:`, e);
    } finally {
      setLoading(false);
    }
  }, []);

  const addDependency = useCallback(
    async (pkg: string) => {
      if (!pkg.trim()) return;
      await withLoading(() => addCondaDepWasm(pkg.trim()), "add conda dependency");
    },
    [withLoading],
  );

  const removeDependency = useCallback(
    async (pkg: string) => {
      await withLoading(() => removeCondaDepWasm(pkg), "remove conda dependency");
    },
    [withLoading],
  );

  // Remove the entire conda dependency section from notebook metadata
  const clearAllDependencies = useCallback(async () => {
    await withLoading(() => clearCondaSection(), "clear conda dependencies");
  }, [withLoading]);

  const setChannels = useCallback(
    async (channels: string[]) => {
      await withLoading(() => setCondaChannelsWasm(channels), "set channels");
    },
    [withLoading],
  );

  const setPython = useCallback(
    async (version: string | null) => {
      await withLoading(() => setCondaPythonWasm(version), "set python version");
    },
    [withLoading],
  );

  const hasDependencies = dependencies !== null && dependencies.dependencies.length > 0;

  // True if conda metadata exists (even with empty deps)
  const isCondaConfigured = dependencies !== null;

  return {
    dependencies,
    hasDependencies,
    isCondaConfigured,
    loading,
    addDependency,
    removeDependency,
    clearAllDependencies,
    setChannels,
    setPython,
    // environment.yml support
    environmentYmlInfo,
    environmentYmlDeps,
  };
}
