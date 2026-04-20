import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { defineConfig, type Plugin } from "vite-plus";

/**
 * Stub plugin for virtual:renderer-plugin/* modules.
 *
 * The notebook app uses a Vite plugin that builds renderer plugins (markdown,
 * plotly, vega, leaflet, sift) as CJS bundles for the isolated iframe.
 * The live-viewer doesn't use iframe isolation, so these virtual modules
 * are stubbed to export empty strings. OutputArea with isolated={false}
 * never calls the loader, but Rolldown still needs the modules resolvable.
 *
 * COUPLING EDGE: This is the first hard boundary between the live-viewer
 * and the notebook app's build system. The iframe-libraries.ts module has
 * dynamic imports to virtual modules that only the notebook app provides.
 */
function rendererPluginStubs(): Plugin {
  const PREFIX = "virtual:renderer-plugin/";
  const RESOLVED_PREFIX = "\0virtual:renderer-plugin/";
  const ISOLATED_ID = "virtual:isolated-renderer";
  const RESOLVED_ISOLATED_ID = "\0virtual:isolated-renderer";
  return {
    name: "live-viewer:renderer-plugin-stubs",
    resolveId(id) {
      if (id.startsWith(PREFIX)) {
        return RESOLVED_PREFIX + id.slice(PREFIX.length);
      }
      if (id === ISOLATED_ID) {
        return RESOLVED_ISOLATED_ID;
      }
    },
    load(id) {
      if (id.startsWith(RESOLVED_PREFIX)) {
        return 'export const code = ""; export const css = "";';
      }
      if (id === RESOLVED_ISOLATED_ID) {
        return 'export const rendererCode = ""; export const rendererCss = "";';
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), rendererPluginStubs()],
  resolve: {
    alias: {
      "@/": path.resolve(__dirname, "../../src") + "/",
      "~/": path.resolve(__dirname, "./src") + "/",
      "runtimed-wasm": path.resolve(__dirname, "../notebook/src/wasm/runtimed-wasm"),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    port: 5175,
    proxy: {
      "/api": "http://localhost:8743",
      "/ws": {
        target: "ws://localhost:8743",
        ws: true,
      },
    },
  },
});
