import { defineConfig } from "vitest/config";
import path from "path";
import type { Plugin } from "vite";

/**
 * Mirror of vegaRawPlugin from vite.config.ts for vitest.
 * Resolves vega-raw/vega-lite-raw/vega-embed-raw to the UMD builds with ?raw.
 */
function vegaRawPlugin(nodeModulesDir: string): Plugin {
  const mapping: Record<string, string> = {
    "vega-raw": path.join(nodeModulesDir, "vega/build/vega.min.js"),
    "vega-lite-raw": path.join(
      nodeModulesDir,
      "vega-lite/build/vega-lite.min.js",
    ),
    "vega-embed-raw": path.join(
      nodeModulesDir,
      "vega-embed/build/vega-embed.min.js",
    ),
    "leaflet-js-raw": path.join(nodeModulesDir, "leaflet/dist/leaflet.js"),
    "leaflet-css-raw": path.join(nodeModulesDir, "leaflet/dist/leaflet.css"),
  };
  return {
    name: "vega-raw-resolve",
    resolveId(source) {
      const filePath = mapping[source];
      if (filePath) return `${filePath}?raw`;
      return null;
    },
  };
}

export default defineConfig({
  plugins: [vegaRawPlugin(path.resolve(__dirname, "./node_modules"))],
  test: {
    environment: "jsdom",
    include: [
      "src/**/__tests__/**/*.test.{ts,tsx}",
      "apps/notebook/src/**/__tests__/**/*.test.{ts,tsx}",
      "packages/**/tests/**/*.test.{ts,tsx}",
    ],
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
