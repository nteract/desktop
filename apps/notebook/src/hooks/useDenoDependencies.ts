import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";
import {
  setDenoFlexibleNpmImports as setDenoFlexibleWasm,
  useDenoFlexibleNpmImports,
} from "../lib/notebook-metadata";

export interface DenoConfigInfo {
  path: string;
  relative_path: string;
  name: string | null;
  has_imports: boolean;
  has_tasks: boolean;
}

export function useDenoDependencies() {
  const [denoAvailable, setDenoAvailable] = useState<boolean | null>(null);
  const [denoConfigInfo, setDenoConfigInfo] = useState<DenoConfigInfo | null>(
    null,
  );
  // Reactive read from the WASM Automerge doc via useSyncExternalStore.
  // Re-renders automatically when the doc changes (bootstrap, sync, writes).
  const flexibleNpmImportsFromDoc = useDenoFlexibleNpmImports();
  const flexibleNpmImports = flexibleNpmImportsFromDoc ?? true;

  // Check Deno availability and detect config on mount
  useEffect(() => {
    invoke<boolean>("check_deno_available").then(setDenoAvailable);
    invoke<DenoConfigInfo | null>("detect_deno_config").then(setDenoConfigInfo);
  }, []);

  const setFlexibleNpmImports = useCallback(async (enabled: boolean) => {
    try {
      await setDenoFlexibleWasm(enabled);
    } catch (e) {
      logger.error("Failed to set flexible npm imports:", e);
    }
  }, []);

  return {
    denoAvailable,
    denoConfigInfo,
    flexibleNpmImports,
    setFlexibleNpmImports,
  };
}
