import { useCallback } from "react";

import { logger } from "../lib/logger";
import {
  setDenoFlexibleNpmImports as setDenoFlexibleWasm,
  useDenoFlexibleNpmImports,
} from "../lib/notebook-metadata";

/**
 * Shape kept for components that used to render "Using deno.json (name)"
 * against a daemon-reported config. The daemon never detected deno.json
 * for anything load-bearing; the field is inlined here as `null` so
 * render sites fall through to the "No deno.json found" branch, which
 * was already the default-looking UI anyway.
 *
 * When deno kernels grow a real reason to sync config into the UI, this
 * hook should read it from `RuntimeStateDoc.project_context` the same
 * way `useDependencies` / `useCondaDependencies` / `usePixiDetection` do.
 */
export interface DenoConfigInfo {
  path: string;
  relative_path: string;
  name: string | null;
  has_imports: boolean;
  has_tasks: boolean;
}

export function useDenoConfig() {
  const flexibleNpmImportsFromDoc = useDenoFlexibleNpmImports();
  const flexibleNpmImports = flexibleNpmImportsFromDoc ?? true;

  const setFlexibleNpmImports = useCallback(async (enabled: boolean) => {
    try {
      await setDenoFlexibleWasm(enabled);
    } catch (e) {
      logger.error("Failed to set flexible npm imports:", e);
    }
  }, []);

  return {
    denoConfigInfo: null as DenoConfigInfo | null,
    flexibleNpmImports,
    setFlexibleNpmImports,
  };
}
