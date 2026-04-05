/**
 * On-demand library loading for isolated iframes.
 *
 * Heavy output renderers are NOT bundled into the isolated renderer IIFE.
 * Instead, they are built as renderer plugins — CJS modules loaded via
 * `frame.installRenderer()`. The iframe's plugin loader provides a shared
 * React instance and a registration API. No window globals needed.
 *
 * Each plugin has its own virtual module (`virtual:renderer-plugin/{name}`)
 * so Vite can code-split them into independent chunks that load only when
 * their MIME types appear in cell outputs.
 */

import type { JupyterOutput } from "@/components/cell/jupyter-output";
import type { IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { isVegaMimeType } from "@/components/outputs/vega-mime";

/**
 * Map of MIME types to the renderer plugin name they require.
 * Extend this when adding support for new visualization libraries.
 */
const MIME_PLUGINS: Record<string, string> = {
  "text/markdown": "markdown",
  "application/vnd.plotly.v1+json": "plotly",
  "application/geo+json": "leaflet",
};

function pluginForMime(mime: string): string | undefined {
  if (MIME_PLUGINS[mime]) return MIME_PLUGINS[mime];
  if (isVegaMimeType(mime)) return "vega";
  return undefined;
}

/** Cache of plugin code promises (shared across all iframes). */
const pluginCache = new Map<string, Promise<{ code: string; css?: string }>>();

/**
 * Lazy-load a renderer plugin's code and optional CSS.
 */
function loadPlugin(name: string): Promise<{ code: string; css?: string }> {
  const cached = pluginCache.get(name);
  if (cached) return cached;

  const promise = (async (): Promise<{ code: string; css?: string }> => {
    switch (name) {
      case "markdown": {
        const { code, css } = await import("virtual:renderer-plugin/markdown");
        return { code, css: css || undefined };
      }
      case "plotly": {
        const { code, css } = await import("virtual:renderer-plugin/plotly");
        return { code, css: css || undefined };
      }
      case "vega": {
        const { code, css } = await import("virtual:renderer-plugin/vega");
        return { code, css: css || undefined };
      }
      case "leaflet": {
        const { code, css } = await import("virtual:renderer-plugin/leaflet");
        return { code, css: css || undefined };
      }
      default:
        throw new Error(`Unknown renderer plugin: ${name}`);
    }
  })();

  pluginCache.set(name, promise);
  return promise;
}

/**
 * Scan outputs for MIME types that require a renderer plugin.
 * Returns deduplicated plugin names.
 */
export function getRequiredLibraries(
  outputs: JupyterOutput[],
  selectMimeType: (data: Record<string, unknown>) => string | null,
): string[] {
  const plugins = new Set<string>();
  for (const output of outputs) {
    if (
      output.output_type === "execute_result" ||
      output.output_type === "display_data"
    ) {
      const mime = selectMimeType(output.data);
      if (mime) {
        const plugin = pluginForMime(mime);
        if (plugin) plugins.add(plugin);
      }
    }
  }
  return Array.from(plugins);
}

/**
 * Install required renderer plugins into an iframe.
 * Idempotent per iframe — tracks what has been installed via `injectedSet`.
 */
export async function injectLibraries(
  frame: IsolatedFrameHandle,
  libraryNames: Iterable<string>,
  injectedSet: Set<string>,
): Promise<void> {
  for (const name of libraryNames) {
    if (injectedSet.has(name)) continue;
    const plugin = await loadPlugin(name);
    console.debug(
      `[iframe-libraries] installing renderer plugin "${name}" (${(plugin.code.length / 1024).toFixed(0)}KB)`,
    );
    frame.installRenderer(plugin.code, plugin.css);
    injectedSet.add(name);
  }
}
