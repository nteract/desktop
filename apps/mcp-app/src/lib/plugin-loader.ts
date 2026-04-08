/**
 * Lazy plugin loader for renderer plugins served by the daemon.
 *
 * Heavy visualization renderers (plotly, vega, leaflet) are not bundled
 * in the MCP App HTML. Instead, they're fetched on demand from the
 * daemon's HTTP server at `{blob_base_url}/plugins/{name}.js`.
 *
 * Plugins are CJS modules with an `install(ctx)` export that registers
 * React components for specific MIME types. React is provided via a
 * custom `require` shim — plugins don't bundle their own React.
 */

import { isVegaMimeType } from "./mime-priority";

interface PluginModule {
  code: string;
  css?: string;
}

/**
 * Map MIME type → plugin name served at /plugins/{name}.js
 */
const MIME_TO_PLUGIN: Record<string, string> = {
  "application/vnd.plotly.v1+json": "plotly",
  "application/geo+json": "leaflet",
};

/**
 * Get the plugin name for a MIME type, or undefined if no plugin needed.
 */
function pluginNameForMime(mime: string): string | undefined {
  if (MIME_TO_PLUGIN[mime]) return MIME_TO_PLUGIN[mime];
  if (isVegaMimeType(mime)) return "vega";
  return undefined;
}

/**
 * Check if a MIME type needs a daemon-served plugin to render.
 */
export function needsDaemonPlugin(mime: string): boolean {
  return pluginNameForMime(mime) !== undefined;
}

/** Cache of loaded plugin code, keyed by plugin name. */
const pluginCache = new Map<string, Promise<PluginModule>>();

/**
 * Fetch a plugin's JS (and optional CSS) from the daemon.
 * Caches successful loads; evicts failures for retry.
 */
async function fetchPlugin(
  baseUrl: string,
  pluginName: string,
): Promise<PluginModule> {
  const jsUrl = `${baseUrl}/plugins/${pluginName}.js`;
  const cssUrl = `${baseUrl}/plugins/${pluginName}.css`;

  const jsResponse = await fetch(jsUrl);
  if (!jsResponse.ok) {
    throw new Error(`Plugin fetch failed: ${jsResponse.status} for ${jsUrl}`);
  }
  const code = await jsResponse.text();

  // CSS is optional — try to fetch, ignore 404
  let css: string | undefined;
  try {
    const cssResponse = await fetch(cssUrl);
    if (cssResponse.ok) {
      css = await cssResponse.text();
    }
  } catch {
    // CSS not available, that's fine
  }

  return { code, css };
}

/**
 * Load and install a renderer plugin for the given MIME type.
 *
 * Returns the plugin module (code + css) or undefined if:
 * - The MIME type doesn't need a plugin
 * - No blob_base_url is available (can't reach daemon HTTP server)
 *
 * The plugin is fetched once and cached. Failed fetches are evicted
 * from cache so retries are possible.
 */
export async function loadPluginForMime(
  mime: string,
  blobBaseUrl: string | undefined,
): Promise<PluginModule | undefined> {
  if (!blobBaseUrl) return undefined;

  const pluginName = pluginNameForMime(mime);
  if (!pluginName) return undefined;

  const cached = pluginCache.get(pluginName);
  if (cached) return cached;

  const promise = fetchPlugin(blobBaseUrl, pluginName);

  pluginCache.set(pluginName, promise);
  // Evict on failure so retries work
  promise.catch(() => pluginCache.delete(pluginName));

  return promise;
}
