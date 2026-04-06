/**
 * Shared MIME type → renderer plugin mapping.
 *
 * Used by both the materialization layer (to pre-compute requiredPlugins)
 * and iframe-libraries.ts (to load and install plugins on demand).
 *
 * This module has NO dependencies on React, Vite virtual modules, or
 * iframe APIs — it's pure data mapping, safe to import anywhere.
 */

import { isVegaMimeType } from "@/components/outputs/vega-mime";

/**
 * Map of exact MIME types to the renderer plugin name they require.
 * Extend this when adding support for new visualization libraries.
 */
const MIME_PLUGINS: Record<string, string> = {
  "text/markdown": "markdown",
  "application/vnd.plotly.v1+json": "plotly",
  "application/geo+json": "leaflet",
  "application/vnd.bokehjs_exec.v0+json": "bokeh",
  "application/vnd.bokehjs_load.v0+json": "bokeh",
};

/**
 * Determine which renderer plugin (if any) is needed for a given MIME type.
 * Returns the plugin name (e.g., "plotly", "vega") or undefined if no plugin is needed.
 */
export function pluginForMime(mime: string): string | undefined {
  if (MIME_PLUGINS[mime]) return MIME_PLUGINS[mime];
  if (isVegaMimeType(mime)) return "vega";
  return undefined;
}

/**
 * Scan Jupyter outputs and return the deduplicated set of renderer plugin
 * names required to render them.
 *
 * This is a pure data function — it doesn't load or install anything.
 * Use it at materialization time to pre-compute which plugins a cell needs.
 */
export function computeRequiredPlugins(
  outputs: Array<{
    output_type: string;
    data?: Record<string, unknown>;
  }>,
): string[] {
  const plugins = new Set<string>();
  for (const output of outputs) {
    if (
      output.output_type === "execute_result" ||
      output.output_type === "display_data"
    ) {
      if (output.data) {
        for (const mime of Object.keys(output.data)) {
          const plugin = pluginForMime(mime);
          if (plugin) plugins.add(plugin);
        }
      }
    }
  }
  return Array.from(plugins);
}
