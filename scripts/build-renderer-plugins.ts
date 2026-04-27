/**
 * Build pre-built renderer plugin artifacts.
 *
 * Produces two sets of outputs:
 *   1. apps/notebook/src/renderer-plugins/ : core IIFE + 4 CJS plugins for the notebook app
 *   2. crates/runt-mcp/assets/plugins/     : MCP-wrapped plugins for the daemon's MCP server
 *
 * Both directories are gitignored. CI rebuilds them as a prerequisite step
 * before any job that compiles the runtimed daemon (which `include_bytes!`s
 * them) or runs the notebook Vite build (which loads them at runtime).
 *
 * Run locally after a fresh clone, or when renderer source or vendored
 * libraries change:
 *
 *   cargo xtask renderer-plugins
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { build } from "vite-plus";
import {
  buildAllRendererPlugins,
  RENDERER_PLUGINS,
} from "../src/build/renderer-plugin-builder.ts";
import { wrapForMcpApp } from "../apps/mcp-app/src/lib/wrap-plugin.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");

const notebookPluginDir = path.join(repoRoot, "apps/notebook/src/renderer-plugins");
const mcpPluginDir = path.join(repoRoot, "crates/runt-mcp/assets/plugins");

async function buildCoreIIFE(): Promise<{ code: string; css: string }> {
  const srcDir = path.join(repoRoot, "src");
  const nodeModules = path.join(repoRoot, "node_modules");

  const result = await build({
    configFile: false,
    mode: "production",
    plugins: [
      tailwindcss(),
      {
        name: "vega-raw-resolve",
        resolveId(source: string) {
          const mapping: Record<string, string> = {
            "vega-raw": path.join(nodeModules, "vega/build/vega.min.js"),
            "vega-lite-raw": path.join(nodeModules, "vega-lite/build/vega-lite.min.js"),
            "vega-embed-raw": path.join(nodeModules, "vega-embed/build/vega-embed.min.js"),
            "leaflet-js-raw": path.join(nodeModules, "leaflet/dist/leaflet.js"),
            "leaflet-css-raw": path.join(nodeModules, "leaflet/dist/leaflet.css"),
          };
          const filePath = mapping[source];
          if (filePath) return `${filePath}?raw`;
          return null;
        },
      },
    ],
    esbuild: { jsx: "automatic", jsxImportSource: "react", jsxDev: false },
    resolve: { alias: { "@/": `${srcDir}/` } },
    build: {
      write: false,
      lib: {
        entry: path.join(srcDir, "isolated-renderer/index.tsx"),
        name: "IsolatedRenderer",
        formats: ["iife"],
        fileName: () => "isolated-renderer.js",
      },
      rolldownOptions: {
        output: { assetFileNames: "isolated-renderer.[ext]" },
        external: [
          "@tauri-apps/api",
          "@tauri-apps/plugin-shell",
          "@tauri-apps/plugin-fs",
          /^@tauri-apps\/.*/,
        ],
        onwarn(warning, warn) {
          if (
            warning.code === "MODULE_LEVEL_DIRECTIVE" &&
            warning.message?.includes('"use client"')
          )
            return;
          warn(warning);
        },
      },
      minify: true,
      sourcemap: false,
    },
    define: { "process.env.NODE_ENV": JSON.stringify("production") },
    logLevel: "warn",
  });

  let code = "";
  let css = "";
  const outputs = Array.isArray(result) ? result : [result];
  for (const output of outputs) {
    if ("output" in output) {
      for (const chunk of output.output) {
        if (chunk.type === "chunk" && chunk.fileName.endsWith(".js")) {
          code = chunk.code;
        } else if (chunk.type === "asset" && chunk.fileName.endsWith(".css")) {
          css =
            typeof chunk.source === "string"
              ? chunk.source
              : new TextDecoder().decode(chunk.source);
        }
      }
    }
  }

  if (!code) throw new Error("Failed to build isolated renderer IIFE");
  return { code, css };
}

async function main() {
  fs.mkdirSync(notebookPluginDir, { recursive: true });
  fs.mkdirSync(mcpPluginDir, { recursive: true });

  // Build core IIFE and renderer plugins in parallel
  const [iife, plugins] = await Promise.all([buildCoreIIFE(), buildAllRendererPlugins(RENDERER_PLUGINS)]);

  // Write core IIFE (notebook only — MCP doesn't use the IIFE)
  fs.writeFileSync(path.join(notebookPluginDir, "isolated-renderer.js"), iife.code);
  fs.writeFileSync(path.join(notebookPluginDir, "isolated-renderer.css"), iife.css);
  console.log(
    `  isolated-renderer: ${(iife.code.length / 1024).toFixed(0)} kB JS, ${(iife.css.length / 1024).toFixed(0)} kB CSS`,
  );

  // Write renderer plugins (both notebook and MCP)
  for (const { name, code, css } of plugins) {
    // Notebook: raw CJS
    fs.writeFileSync(path.join(notebookPluginDir, `${name}.js`), code);
    if (css) fs.writeFileSync(path.join(notebookPluginDir, `${name}.css`), css);

    // MCP: wrapped for MCP app
    const wrapped = wrapForMcpApp(code);
    fs.writeFileSync(path.join(mcpPluginDir, `${name}.js`), wrapped);
    if (css) fs.writeFileSync(path.join(mcpPluginDir, `${name}.css`), css);

    const sizeParts = [`${(code.length / 1024).toFixed(0)} kB JS`];
    if (css) sizeParts.push(`${(css.length / 1024).toFixed(0)} kB CSS`);
    console.log(`  ${name}: ${sizeParts.join(", ")}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
