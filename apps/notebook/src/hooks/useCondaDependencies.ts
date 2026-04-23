import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";
import {
  addCondaDependency as addCondaDepWasm,
  clearCondaSection,
  removeCondaDependency as removeCondaDepWasm,
  setCondaChannels as setCondaChannelsWasm,
  setCondaPython as setCondaPythonWasm,
  useCondaDeps,
} from "../lib/notebook-metadata";

export interface CondaDependencies {
  dependencies: string[];
  channels: string[];
  python: string | null;
}

/** Info about a detected environment.yml */
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

/** Full environment.yml dependencies for display */
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

export function useCondaDependencies() {
  const [loading, setLoading] = useState(false);

  // environment.yml detection state
  const [environmentYmlInfo, setEnvironmentYmlInfo] = useState<EnvironmentYmlInfo | null>(null);
  const [environmentYmlDeps, setEnvironmentYmlDeps] = useState<EnvironmentYmlDeps | null>(null);

  // Reactive read from the WASM Automerge doc via useSyncExternalStore.
  // Re-renders automatically when the doc changes (bootstrap, sync, writes).
  const condaDeps = useCondaDeps();
  const dependencies = condaDeps
    ? {
        dependencies: condaDeps.dependencies,
        channels: condaDeps.channels,
        python: condaDeps.python,
      }
    : null;

  // Load full environment.yml dependencies
  const loadEnvironmentYmlDeps = useCallback(async () => {
    try {
      const deps = await invoke<EnvironmentYmlDeps | null>("get_environment_yml_dependencies");
      setEnvironmentYmlDeps(deps);
    } catch (e) {
      logger.error("Failed to load environment.yml dependencies:", e);
    }
  }, []);

  // Detect environment.yml on mount
  useEffect(() => {
    invoke<EnvironmentYmlInfo | null>("detect_environment_yml").then(setEnvironmentYmlInfo);
  }, []);

  // Load environment.yml deps when we detect one
  useEffect(() => {
    if (environmentYmlInfo?.has_dependencies) {
      loadEnvironmentYmlDeps();
    }
  }, [environmentYmlInfo, loadEnvironmentYmlDeps]);

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
