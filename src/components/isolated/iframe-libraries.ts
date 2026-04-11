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
 *
 * This module is the single source of truth for the MIME type → virtual
 * module mapping. Callers pass MIME types directly — no intermediate
 * "plugin name" concept.
 */

import type { IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { isVegaMimeType } from "@/components/outputs/vega-mime";

interface PluginModule {
  code: string;
  css?: string;
}

/** Normalize a raw virtual module import to { code, css? }. */
function normalize(m: { code: string; css?: string }): PluginModule {
  return { code: m.code, css: m.css || undefined };
}

/**
 * Canonical map: MIME type → lazy loader for the virtual module.
 * Extend this when adding support for new visualization libraries.
 */
const PLUGIN_MIME_TYPES: Record<string, () => Promise<PluginModule>> = {
  "text/markdown": () => import("virtual:renderer-plugin/markdown").then(normalize),
  "text/latex": () => import("virtual:renderer-plugin/markdown").then(normalize),
  "application/vnd.plotly.v1+json": () => import("virtual:renderer-plugin/plotly").then(normalize),
  "application/geo+json": () => import("virtual:renderer-plugin/leaflet").then(normalize),
};

/** Lazy loader for all Vega/Vega-Lite MIME variants. */
const loadVega = () => import("virtual:renderer-plugin/vega").then(normalize);

/**
 * Check whether a MIME type requires a renderer plugin.
 */
export function needsPlugin(mime: string): boolean {
  return mime in PLUGIN_MIME_TYPES || isVegaMimeType(mime);
}

/** Cache of plugin code promises, keyed by MIME type. */
const pluginCache = new Map<string, Promise<PluginModule>>();

/**
 * Load the renderer plugin for a MIME type.
 * Returns undefined if the MIME type doesn't need a plugin.
 * Deduplicates concurrent loads for the same MIME type.
 */
function loadPluginForMime(mime: string): Promise<PluginModule> | undefined {
  const cached = pluginCache.get(mime);
  if (cached) return cached;

  const loader = PLUGIN_MIME_TYPES[mime] ?? (isVegaMimeType(mime) ? loadVega : undefined);
  if (!loader) return undefined;

  const promise = loader();
  pluginCache.set(mime, promise);
  return promise;
}

/**
 * Pre-warm the plugin cache for the given MIME types.
 * Kicks off virtual module fetches so later injection resolves instantly.
 */
export function preWarmForMimes(mimes: Iterable<string>): void {
  for (const mime of mimes) {
    loadPluginForMime(mime);
  }
}

/**
 * Install renderer plugins required by the given MIME types into an iframe.
 * Idempotent per iframe — tracks what has been installed via `injectedSet`.
 */
export async function injectPluginsForMimes(
  frame: IsolatedFrameHandle,
  mimes: Iterable<string>,
  injectedSet: Set<string>,
): Promise<void> {
  for (const mime of mimes) {
    if (injectedSet.has(mime)) continue;
    const plugin = await loadPluginForMime(mime);
    if (!plugin) continue;
    console.debug(
      `[iframe-libraries] installing renderer plugin for "${mime}" (${(plugin.code.length / 1024).toFixed(0)}KB)`,
    );
    frame.installRenderer(plugin.code, plugin.css);
    injectedSet.add(mime);
  }
}
