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
  dependencies: string[];
  has_dependencies: boolean;
  dependency_count: number;
  pypi_dependencies: string[];
  has_pypi_dependencies: boolean;
  pypi_dependency_count: number;
  python: string | null;
  channels: string[];
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
 * Derive `PixiInfo` from a `ProjectContext`. Pure; exported for tests.
 *
 * Returns `null` for every non-Detected state and for Detected pointing
 * at a non-pixi project file.
 */
export function derivePixiInfo(ctx: ProjectContext): PixiInfo | null {
  if (ctx.state !== "Detected") return null;
  if (ctx.project_file.kind !== "PixiToml") return null;
  const { channels, pypi_dependencies } = pixiExtras(ctx.parsed.extras);
  return {
    path: ctx.project_file.absolute_path,
    relative_path: ctx.project_file.relative_to_notebook,
    workspace_name: null,
    dependencies: ctx.parsed.dependencies,
    has_dependencies: ctx.parsed.dependencies.length > 0,
    dependency_count: ctx.parsed.dependencies.length,
    pypi_dependencies,
    has_pypi_dependencies: pypi_dependencies.length > 0,
    pypi_dependency_count: pypi_dependencies.length,
    python: ctx.parsed.requires_python,
    channels,
  };
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
  const pixiInfo = useMemo<PixiInfo | null>(
    () => derivePixiInfo(runtimeState.project_context),
    [runtimeState.project_context],
  );

  return {
    pixiInfo,
  };
}
