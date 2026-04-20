import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { logger } from "../lib/logger";
import type { PixiInfo } from "../types";

/**
 * Hook for pixi.toml detection.
 *
 * Detects pixi.toml near the notebook and returns its info.
 * Pixi dependencies are managed via `pixi add`/`pixi remove` in the terminal,
 * not through the notebook metadata — pixi.toml is the source of truth.
 */
export function usePixiDetection() {
  const [pixiInfo, setPixiInfo] = useState<PixiInfo | null>(null);

  useEffect(() => {
    invoke<PixiInfo | null>("detect_pixi_toml")
      .then(setPixiInfo)
      .catch((e) => {
        logger.debug("[pixi] Failed to detect pixi.toml:", e);
      });
  }, []);

  return {
    pixiInfo,
  };
}
