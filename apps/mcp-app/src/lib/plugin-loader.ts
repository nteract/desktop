/**
 * Lazy plugin loader for renderer plugins served by the daemon.
 *
 * Heavy visualization renderers (plotly, vega, leaflet) are not bundled
 * in the MCP App HTML. Instead, they're loaded via <script> tags from
 * the daemon's HTTP server at `{blob_base_url}/plugins/{name}.js`.
 *
 * Uses <script src="..."> instead of fetch+eval to work within the
 * host's CSP (no `unsafe-eval` needed — the daemon origin is already
 * in the CSP `resourceDomains` allowlist).
 */

import { isVegaMimeType } from "./mime-priority";
import { installPluginFromUrl } from "./plugin-executor";

interface PluginInfo {
  name: string;
  /** Whether this plugin has a separate CSS file */
  hasCss: boolean;
}

const MIME_TO_PLUGIN: Record<string, PluginInfo> = {
  "text/markdown": { name: "markdown", hasCss: true },
  "text/latex": { name: "markdown", hasCss: true },
  "application/vnd.plotly.v1+json": { name: "plotly", hasCss: false },
  "application/geo+json": { name: "leaflet", hasCss: true },
};

const VEGA_PLUGIN: PluginInfo = { name: "vega", hasCss: false };

function pluginInfoForMime(mime: string): PluginInfo | undefined {
  if (MIME_TO_PLUGIN[mime]) return MIME_TO_PLUGIN[mime];
  if (isVegaMimeType(mime)) return VEGA_PLUGIN;
  return undefined;
}

/**
 * Check if a MIME type needs a daemon-served plugin to render.
 */
export function needsDaemonPlugin(mime: string): boolean {
  return pluginInfoForMime(mime) !== undefined;
}

/** Track which plugins have been loaded (by name). */
const loadedPlugins = new Set<string>();
/** In-flight loads to deduplicate concurrent requests. */
const loadingPlugins = new Map<string, Promise<void>>();

/**
 * Load and install a renderer plugin for the given MIME type.
 * Uses <script> tag loading (CSP-compatible, no unsafe-eval).
 *
 * Returns a promise that resolves when the plugin is installed,
 * or undefined if the MIME type doesn't need a plugin.
 */
export function loadPluginForMime(
  mime: string,
  blobBaseUrl: string | undefined,
): Promise<void> | undefined {
  if (!blobBaseUrl) return undefined;

  const info = pluginInfoForMime(mime);
  if (!info) return undefined;

  // Already loaded
  if (loadedPlugins.has(info.name)) return Promise.resolve();

  // Already loading — deduplicate
  const existing = loadingPlugins.get(info.name);
  if (existing) return existing;

  const jsUrl = `${blobBaseUrl}/plugins/${info.name}.js`;
  const cssUrl = info.hasCss ? `${blobBaseUrl}/plugins/${info.name}.css` : undefined;

  const promise = installPluginFromUrl(jsUrl, cssUrl)
    .then(() => {
      loadedPlugins.add(info.name);
      loadingPlugins.delete(info.name);
    })
    .catch((err) => {
      loadingPlugins.delete(info.name);
      throw err;
    });

  loadingPlugins.set(info.name, promise);
  return promise;
}
