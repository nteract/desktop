import fs from "node:fs";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { defineConfig, type Plugin } from "vite-plus";

/**
 * Loads pre-built renderer plugin artifacts from the notebook app's
 * checked-in renderer-plugins/ directory and exposes them as virtual modules.
 *
 * This enables the live-viewer to use the full OutputArea with iframe
 * isolation for rich outputs (plotly, vega, markdown/LaTeX, leaflet, sift).
 *
 * COUPLING EDGE: The live-viewer depends on pre-built artifacts from the
 * notebook app's build pipeline (cargo xtask renderer-plugins). If those
 * artifacts are missing, we fall back to empty stubs (in-DOM rendering only).
 */
function rendererPlugins(): Plugin {
  const PREFIX = "virtual:renderer-plugin/";
  const RESOLVED_PREFIX = "\0virtual:renderer-plugin/";
  const ISOLATED_ID = "virtual:isolated-renderer";
  const RESOLVED_ISOLATED_ID = "\0virtual:isolated-renderer";

  const PREBUILT_DIR = path.resolve(__dirname, "../notebook/src/renderer-plugins");

  function readArtifact(name: string, ext: string): string {
    const file = path.join(PREBUILT_DIR, `${name}.${ext}`);
    try {
      return fs.readFileSync(file, "utf-8");
    } catch {
      return "";
    }
  }

  return {
    name: "live-viewer:renderer-plugins",
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
        const name = id.slice(RESOLVED_PREFIX.length);
        const code = readArtifact(name, "js");
        const css = readArtifact(name, "css");
        return `export const code = ${JSON.stringify(code)};\nexport const css = ${JSON.stringify(css)};`;
      }
      if (id === RESOLVED_ISOLATED_ID) {
        const rendererCode = readArtifact("isolated-renderer", "js");
        const rendererCss = readArtifact("isolated-renderer", "css");
        return `export const rendererCode = ${JSON.stringify(rendererCode)};\nexport const rendererCss = ${JSON.stringify(rendererCss)};`;
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), rendererPlugins()],
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
