/**
 * Vite plugin that loads library files as raw strings with sourcemap comments stripped.
 *
 * Vega v6+ packages use restrictive "exports" fields that block deep imports
 * like `vega/build/vega.min.js?raw`. This plugin resolves virtual module names
 * (e.g. "vega-raw") to the actual files, reads them from disk, strips any
 * `//# sourceMappingURL=...` directives, and returns the content as a default
 * export string.
 *
 * Stripping sourcemap comments prevents iframes from making 404 network requests
 * for `.map` files that don't exist at the blob URL origin. See #1464.
 */

import fs from "fs/promises";
import path from "path";
import type { Plugin } from "vite-plus";

/** Matches JS-style sourceMappingURL comments: //# sourceMappingURL=foo.js.map */
const SOURCEMAP_JS = /\/\/[#@]\s*sourceMappingURL=\S+/g;
/** Matches CSS-style sourceMappingURL comments: /*# sourceMappingURL=foo.css.map *​/ */
const SOURCEMAP_CSS = /\/\*[#@]\s*sourceMappingURL=\S+\s*\*\//g;

const JS_SUFFIX = ".js";
// Resolve to a path-shaped ID instead of a virtual/protocol-like ID. The
// synthetic JS suffix keeps CSS files out of Vite's CSS pipeline, while the
// query lets Vitest dynamic imports stay inside Vite's loader.
const QUERY = "?raw-lib";

export function rawLibPlugin(nodeModulesDir: string): Plugin {
  const mapping: Record<string, string> = {
    "vega-raw": path.join(nodeModulesDir, "vega/build/vega.min.js"),
    "vega-lite-raw": path.join(nodeModulesDir, "vega-lite/build/vega-lite.min.js"),
    "vega-embed-raw": path.join(nodeModulesDir, "vega-embed/build/vega-embed.min.js"),
    "plotly-raw": path.join(nodeModulesDir, "plotly.js-dist-min/plotly.min.js"),
    "leaflet-js-raw": path.join(nodeModulesDir, "leaflet/dist/leaflet.js"),
    "leaflet-css-raw": path.join(nodeModulesDir, "leaflet/dist/leaflet.css"),
  };

  return {
    name: "raw-lib",
    resolveId(source) {
      const filePath = mapping[source];
      if (filePath) return `${filePath}${JS_SUFFIX}${QUERY}`;
      return null;
    },
    async load(id) {
      const suffix = `${JS_SUFFIX}${QUERY}`;
      if (!id.endsWith(suffix)) return null;
      const filePath = id.slice(0, -suffix.length);
      let content = await fs.readFile(filePath, "utf-8");
      content = content.replace(SOURCEMAP_JS, "").replace(SOURCEMAP_CSS, "");
      return `export default ${JSON.stringify(content)};`;
    },
  };
}
