import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";

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
  const [flexibleNpmImports, setFlexibleNpmImportsState] =
    useState<boolean>(true);

  // Load the flexible npm imports setting from notebook metadata
  const loadFlexibleNpmImports = useCallback(async () => {
    try {
      const flexible = await invoke<boolean>("get_deno_flexible_npm_imports");
      setFlexibleNpmImportsState(flexible);
    } catch (e) {
      logger.error("Failed to load flexible npm imports:", e);
    }
  }, []);

  // Check Deno availability, detect config, and load settings on mount
  useEffect(() => {
    const init = async () => {
      try {
        const available = await invoke<boolean>("check_deno_available");
        setDenoAvailable(available);

        const config = await invoke<DenoConfigInfo | null>(
          "detect_deno_config",
        );
        setDenoConfigInfo(config);

        await loadFlexibleNpmImports();
      } catch (e) {
        logger.error("Failed to initialize Deno dependencies:", e);
      }
    };
    init();
  }, [loadFlexibleNpmImports]);

  // Re-load when metadata is synced from another window
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlisten = webview.listen("notebook:metadata_updated", () => {
      loadFlexibleNpmImports();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [loadFlexibleNpmImports]);

  const setFlexibleNpmImports = useCallback(async (enabled: boolean) => {
    try {
      await invoke("set_deno_flexible_npm_imports", { enabled });
      setFlexibleNpmImportsState(enabled);
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
