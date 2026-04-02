import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { visualizer } from "rollup-plugin-visualizer";
import { defineConfig, type Plugin } from "vite";
import { isolatedRendererPlugin } from "./vite-plugin-isolated-renderer";

/**
 * Vega packages (v6+) use restrictive "exports" fields that block deep imports
 * like `vega/build/vega.min.js?raw`. This plugin resolves virtual module names
 * (vega-raw, vega-lite-raw, vega-embed-raw) to the actual UMD build files with
 * the ?raw suffix so they load as strings for iframe injection.
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

export default defineConfig(({ command }) => {
  const debugBundleSourceMapsEnabled =
    process.env.RUNT_NOTEBOOK_DEBUG_BUILD === "1";
  const isolatedRendererSourceMapsEnabled =
    command === "serve" || debugBundleSourceMapsEnabled;

  return {
    plugins: [
      react(),
      tailwindcss(),
      vegaRawPlugin(path.resolve(__dirname, "../../node_modules")),
      isolatedRendererPlugin({
        minify: command !== "serve",
        sourcemap: isolatedRendererSourceMapsEnabled ? "inline" : false,
      }),
      visualizer({
        filename: "dist/stats.html",
        open: false,
        gzipSize: true,
        brotliSize: true,
      }),
    ],
    resolve: {
      alias: {
        "@/": path.resolve(__dirname, "../../src") + "/",
        "~/": path.resolve(__dirname, "./src") + "/",
      },
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
      sourcemap: debugBundleSourceMapsEnabled,
      chunkSizeWarningLimit: 10000,
      rollupOptions: {
        input: {
          main: path.resolve(__dirname, "index.html"),
          onboarding: path.resolve(__dirname, "onboarding/index.html"),
          upgrade: path.resolve(__dirname, "upgrade/index.html"),
          settings: path.resolve(__dirname, "settings/index.html"),
          feedback: path.resolve(__dirname, "feedback/index.html"),
        },
        output: {
          entryFileNames: "assets/[name].js",
          chunkFileNames: "assets/[name].js",
          assetFileNames: "assets/[name].[ext]",
        },
      },
    },
    server: {
      port: parseInt(
        process.env.RUNTIMED_VITE_PORT || process.env.CONDUCTOR_PORT || "5174",
      ),
      strictPort: true,
    },
    base: "/",
  };
});
