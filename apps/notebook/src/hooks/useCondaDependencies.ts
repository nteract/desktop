import { useCallback, useMemo, useState } from "react";
import { deriveEnvironmentYml } from "runtimed";
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
export type { EnvironmentYmlDeps, EnvironmentYmlInfo } from "runtimed";

/** Conda sync state — tracks whether declared deps match the running kernel's environment. */
export type CondaSyncState =
  | { status: "not_running" }
  | { status: "not_conda_managed" }
  | { status: "synced" }
  | { status: "dirty" };

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
