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

  // Re-sign the notebook after user modifications to keep it trusted
  const resignTrust = useCallback(async () => {
    try {
      await invoke("approve_notebook_trust");
    } catch (e) {
      // Signing may fail if no trust key yet - that's okay
      logger.debug("[conda] Could not resign trust:", e);
    }
  }, []);

  // Wrap a mutating WASM op with the shared loading + trust-resign + error log
  // shape. `label` appears in the error message so grep still works.
  const withTrustResign = useCallback(
    async (op: () => Promise<void>, label: string) => {
      setLoading(true);
      try {
        await op();
        await resignTrust();
      } catch (e) {
        logger.error(`Failed to ${label}:`, e);
      } finally {
        setLoading(false);
      }
    },
    [resignTrust],
  );

  const addDependency = useCallback(
    async (pkg: string) => {
      if (!pkg.trim()) return;
      await withTrustResign(() => addCondaDepWasm(pkg.trim()), "add conda dependency");
    },
    [withTrustResign],
  );

  const removeDependency = useCallback(
    async (pkg: string) => {
      await withTrustResign(() => removeCondaDepWasm(pkg), "remove conda dependency");
    },
    [withTrustResign],
  );

  // Remove the entire conda dependency section from notebook metadata
  const clearAllDependencies = useCallback(async () => {
    await withTrustResign(() => clearCondaSection(), "clear conda dependencies");
  }, [withTrustResign]);

  const setChannels = useCallback(
    async (channels: string[]) => {
      await withTrustResign(() => setCondaChannelsWasm(channels), "set channels");
    },
    [withTrustResign],
  );

  const setPython = useCallback(
    async (version: string | null) => {
      await withTrustResign(() => setCondaPythonWasm(version), "set python version");
    },
    [withTrustResign],
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
