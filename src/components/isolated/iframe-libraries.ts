/**
 * On-demand library loading for isolated iframes.
 *
 * Heavy libraries are NOT bundled into the isolated renderer IIFE. Instead,
 * they are lazy-loaded from the parent app and injected into iframes only
 * when an output actually needs them.
 *
 * Two injection mechanisms:
 * - **Renderer plugins** (markdown): CJS modules loaded via `frame.installRenderer()`.
 *   The iframe's plugin loader provides a shared React instance and a registration
 *   API. No globals needed.
 * - **Legacy eval libraries** (plotly, vega, leaflet): Raw JS strings injected via
 *   `frame.eval()` that set window globals (e.g., `window.Plotly`). These will
 *   migrate to the renderer plugin API in future PRs.
 */

import type { JupyterOutput } from "@/components/cell/jupyter-output";
import type { IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { isVegaMimeType } from "@/components/outputs/vega-mime";

/**
 * Map of MIME types to the library name they require.
 * Extend this when adding support for new heavy visualization libraries.
 */
const MIME_LIBRARIES: Record<string, string> = {
  "text/markdown": "markdown",
  "application/vnd.plotly.v1+json": "plotly",
  "application/geo+json": "leaflet",
};

/** Libraries that use the renderer plugin API instead of legacy eval. */
const RENDERER_PLUGINS = new Set(["markdown"]);

function libraryForMime(mime: string): string | undefined {
  if (MIME_LIBRARIES[mime]) return MIME_LIBRARIES[mime];
  if (isVegaMimeType(mime)) return "vega";
  return undefined;
}

/** Cache of library code promises (shared across all iframes). */
const codeCache = new Map<string, Promise<{ code: string; css?: string }>>();

/**
 * Lazy-load a library's code (and optional CSS).
 */
function loadLibrary(name: string): Promise<{ code: string; css?: string }> {
  const cached = codeCache.get(name);
  if (cached) return cached;

  const promise = (async (): Promise<{ code: string; css?: string }> => {
    switch (name) {
      case "markdown": {
        const { markdownRendererCode, markdownRendererCss } = await import(
          "virtual:isolated-renderer"
        );
        return {
          code: markdownRendererCode,
          css: markdownRendererCss || undefined,
        };
      }
      case "plotly": {
        const mod = await import("plotly-raw");
        return { code: mod.default };
      }
      case "vega": {
        // Load all three in parallel: vega (runtime), vega-lite (compiler), vega-embed (renderer).
        // Eval order matters: vega-embed expects window.vega and window.vl to exist.
        // These packages use restrictive "exports" fields that block deep ?raw imports,
        // so we use resolve aliases defined in vite.config.ts and vitest.config.ts
        // to bypass the exports restriction and load the UMD builds as raw strings.
        const [vegaMod, vegaLiteMod, vegaEmbedMod] = await Promise.all([
          import("vega-raw"),
          import("vega-lite-raw"),
          import("vega-embed-raw"),
        ]);
        return {
          code: `${vegaMod.default}\n${vegaLiteMod.default}\n${vegaEmbedMod.default}`,
        };
      }
      case "leaflet": {
        // Load Leaflet JS and CSS. Inject CSS via a <style> tag before the JS runs.
        const [leafletJs, leafletCss] = await Promise.all([
          import("leaflet-js-raw"),
          import("leaflet-css-raw"),
        ]);
        const cssInjection = `(function(){var s=document.createElement('style');s.textContent=${JSON.stringify(leafletCss.default)};document.head.appendChild(s);})();`;
        return { code: `${cssInjection}\n${leafletJs.default}` };
      }
      default:
        throw new Error(`Unknown iframe library: ${name}`);
    }
  })();

  codeCache.set(name, promise);
  return promise;
}

/**
 * Scan outputs for MIME types that require a heavy library.
 * Returns deduplicated library names.
 */
export function getRequiredLibraries(
  outputs: JupyterOutput[],
  selectMimeType: (data: Record<string, unknown>) => string | null,
): string[] {
  const libs = new Set<string>();
  for (const output of outputs) {
    if (
      output.output_type === "execute_result" ||
      output.output_type === "display_data"
    ) {
      const mime = selectMimeType(output.data);
      if (mime) {
        const lib = libraryForMime(mime);
        if (lib) libs.add(lib);
      }
    }
  }
  return Array.from(libs);
}

/**
 * Inject required libraries into an iframe.
 * Idempotent per iframe — tracks what has been injected via `injectedSet`.
 *
 * Renderer plugins use `frame.installRenderer()` (CJS module with shared React).
 * Legacy libraries use `frame.eval()` (raw JS setting window globals).
 */
export async function injectLibraries(
  frame: IsolatedFrameHandle,
  libraryNames: Iterable<string>,
  injectedSet: Set<string>,
): Promise<void> {
  for (const name of libraryNames) {
    if (injectedSet.has(name)) continue;
    const lib = await loadLibrary(name);

    if (RENDERER_PLUGINS.has(name)) {
      // Renderer plugin: use the plugin API (CJS + shared React, no globals)
      console.debug(
        `[iframe-libraries] installing renderer plugin "${name}" (${(lib.code.length / 1024).toFixed(0)}KB)`,
      );
      frame.installRenderer(lib.code, lib.css);
    } else {
      // Legacy: eval raw JS that sets window globals
      const guard = `__LIB_${name.toUpperCase()}__`;
      console.debug(
        `[iframe-libraries] injecting "${name}" (${(lib.code.length / 1024).toFixed(0)}KB)`,
      );
      frame.eval(
        `if(!window.${guard}){window.${guard}=true;\n${lib.code}\n}`,
      );
    }
    injectedSet.add(name);
  }
}
