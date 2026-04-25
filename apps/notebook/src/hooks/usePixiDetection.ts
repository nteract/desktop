import { useMemo } from "react";
import type { ProjectContext, ProjectFileExtras } from "runtimed";
import { useRuntimeState } from "../lib/runtime-state";

/**
 * Info about a detected pixi.toml.
 *
 * Derived from `RuntimeState.project_context` by `usePixiDetection()`.
 * `workspace_name` is `null` until the daemon surfaces `[project].name`
 * through `ProjectFileParsed`; the UI today treats it as optional display.
 */
export interface PixiInfo {
  path: string;
  relative_path: string;
  workspace_name: string | null;
  has_dependencies: boolean;
  dependency_count: number;
  has_pypi_dependencies: boolean;
  pypi_dependency_count: number;
  python: string | null;
  channels: string[];
}

/**
 * Narrow `ProjectContext.Detected` to the pixi case and pull out the
 * parsed extras. Any non-pixi state returns `null`.
 */
interface DetectedPixi {
  absolute_path: string;
  relative_path: string;
  dependencies: string[];
  requires_python: string | null;
  channels: string[];
  pypi_dependencies: string[];
}

function detectedPixi(ctx: ProjectContext): DetectedPixi | null {
  if (ctx.state !== "Detected") return null;
  if (ctx.project_file.kind !== "PixiToml") return null;
  const { channels, pypi_dependencies } = pixiExtras(ctx.parsed.extras);
  return {
    absolute_path: ctx.project_file.absolute_path,
    relative_path: ctx.project_file.relative_to_notebook,
    dependencies: ctx.parsed.dependencies,
    requires_python: ctx.parsed.requires_python,
    channels,
    pypi_dependencies,
  };
}

function pixiExtras(extras: ProjectFileExtras): {
  channels: string[];
  pypi_dependencies: string[];
} {
  if (extras.kind === "Pixi") {
    return { channels: extras.channels, pypi_dependencies: extras.pypi_dependencies };
  }
  return { channels: [], pypi_dependencies: [] };
}

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
  const pixiInfo = useMemo<PixiInfo | null>(() => {
    const detected = detectedPixi(runtimeState.project_context);
    if (!detected) return null;
    return {
      path: detected.absolute_path,
      relative_path: detected.relative_path,
      workspace_name: null,
      has_dependencies: detected.dependencies.length > 0,
      dependency_count: detected.dependencies.length,
      has_pypi_dependencies: detected.pypi_dependencies.length > 0,
      pypi_dependency_count: detected.pypi_dependencies.length,
      python: detected.requires_python,
      channels: detected.channels,
    };
  }, [runtimeState.project_context]);

  return {
    pixiInfo,
  };
}
