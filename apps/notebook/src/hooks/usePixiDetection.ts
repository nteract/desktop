import { useMemo } from "react";
import { derivePixiInfo } from "runtimed";
import { useRuntimeState } from "../lib/runtime-state";
export type { PixiInfo } from "runtimed";

/**
 * Hook for pixi.toml detection.
 *
 * Reads `RuntimeState.project_context` (daemon-populated, see #2208). No
 * filesystem walk; no Tauri command. Pixi dependencies are managed via
 * `pixi add`/`pixi remove` in the terminal — pixi.toml is the source
 * of truth — so this hook is display-only.
 */
export function usePixiDetection() {
  const runtimeState = useRuntimeState();
  const pixiInfo = useMemo(
    () => derivePixiInfo(runtimeState.project_context),
    [runtimeState.project_context],
  );

  return {
    pixiInfo,
  };
}
