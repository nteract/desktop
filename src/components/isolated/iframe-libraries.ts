/**
 * On-demand library loading for isolated iframes.
 *
 * Heavy libraries (plotly.js, etc.) are NOT bundled into the isolated renderer
 * IIFE. Instead, they are lazy-loaded as raw strings from the parent app and
 * injected into iframes via `eval` only when an output actually needs them.
 *
 * The eval message is processed synchronously by the iframe's bootstrap before
 * any subsequent render messages, so the library global (e.g. `window.Plotly`)
 * is guaranteed to be available when the output component mounts.
 */

import type { JupyterOutput } from "@/components/cell/jupyter-output";
import type { IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { isVegaMimeType } from "@/components/outputs/vega-mime";

/**
 * Map of MIME types to the library name they require.
 * Extend this when adding support for new heavy visualization libraries.
 */
const MIME_LIBRARIES: Record<string, string> = {
  "application/vnd.plotly.v1+json": "plotly",
  "application/geo+json": "leaflet",
};

function libraryForMime(mime: string): string | undefined {
  if (MIME_LIBRARIES[mime]) return MIME_LIBRARIES[mime];
  if (isVegaMimeType(mime)) return "vega";
  return undefined;
}

/** Cache of library code promises (shared across all iframes). */
const codeCache = new Map<string, Promise<string>>();

/**
 * Lazy-load a library's source code as a raw string.
 * The returned string is a self-contained script that sets a global
 * (e.g. `window.Plotly`) when eval'd.
 */
function loadLibraryCode(name: string): Promise<string> {
  const cached = codeCache.get(name);
  if (cached) return cached;

  const promise = (async (): Promise<string> => {
    switch (name) {
      case "plotly": {
        const mod = await import("plotly-raw");
        return mod.default;
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
        return `${vegaMod.default}\n${vegaLiteMod.default}\n${vegaEmbedMod.default}`;
      }
      case "leaflet": {
        // Load Leaflet JS and CSS. Inject CSS via a <style> tag before the JS runs.
        const [leafletJs, leafletCss] = await Promise.all([
          import("leaflet-js-raw"),
          import("leaflet-css-raw"),
        ]);
        const cssInjection = `(function(){var s=document.createElement('style');s.textContent=${JSON.stringify(leafletCss.default)};document.head.appendChild(s);})();`;
        return `${cssInjection}\n${leafletJs.default}`;
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
 * Inject required libraries into an iframe via eval.
 * Idempotent per iframe — tracks what has been injected via `injectedSet`.
 */
export async function injectLibraries(
  frame: IsolatedFrameHandle,
  libraryNames: Iterable<string>,
  injectedSet: Set<string>,
): Promise<void> {
  for (const name of libraryNames) {
    if (injectedSet.has(name)) continue;
    const code = await loadLibraryCode(name);
    // Idempotent guard inside the iframe (belt + suspenders with injectedSet)
    const guard = `__LIB_${name.toUpperCase()}__`;
    frame.eval(
      `if(!window.${guard}){window.${guard}=true;\n${code}\n}`,
    );
    injectedSet.add(name);
  }
}
