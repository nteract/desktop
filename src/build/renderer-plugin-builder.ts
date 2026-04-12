/**
 * Shared renderer plugin builder
 *
 * Centralizes the build logic for renderer plugins (markdown, plotly, vega, leaflet)
 * used by both the notebook Vite plugin and MCP app build scripts.
 *
 * Plugins are built as CJS with React externalized (React is provided by the
 * isolated renderer's IIFE). Always minified — these contain entire libraries
 * (plotly alone is 20MB unminified).
 */

import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { build } from "vite-plus";

/**
 * Definition of a renderer plugin to build
 */
export interface RendererPluginDef {
  name: string;
  entry: string;
}

/**
 * Output from building a renderer plugin
 */
export interface RendererPluginOutput {
  name: string;
  code: string;
  css: string;
}

/**
 * Get the absolute path to the src directory (parent of build/)
 */
function getSrcDir(): string {
  if (typeof import.meta.dirname !== "undefined") {
    return path.resolve(import.meta.dirname, "..");
  }
  if (typeof import.meta.url !== "undefined") {
    const __filename = fileURLToPath(import.meta.url);
    const __dirname = path.dirname(__filename);
    return path.resolve(__dirname, "..");
  }
  throw new Error("Unable to resolve source directory: import.meta not available");
}

const srcDir = getSrcDir();

export const RENDERER_PLUGINS: RendererPluginDef[] = [
  { name: "markdown", entry: path.resolve(srcDir, "isolated-renderer/markdown-renderer.tsx") },
  { name: "plotly", entry: path.resolve(srcDir, "isolated-renderer/plotly-renderer.tsx") },
  { name: "vega", entry: path.resolve(srcDir, "isolated-renderer/vega-renderer.tsx") },
  { name: "leaflet", entry: path.resolve(srcDir, "isolated-renderer/leaflet-renderer.tsx") },
];

/**
 * Extract JS and CSS from Vite build output
 */
export function extractBuildOutput(result: unknown, label: string): { code: string; css: string } {
  let code = "";
  let css = "";

  const outputs = Array.isArray(result) ? result : [result];
  for (const output of outputs) {
    if (output && typeof output === "object" && "output" in output) {
      const buildOutput = output.output as Array<{
        type: string;
        fileName: string;
        code?: string;
        source?: string | Uint8Array;
      }>;
      for (const chunk of buildOutput) {
        if (chunk.type === "chunk" && chunk.fileName.endsWith(".js")) {
          code = chunk.code || "";
        } else if (chunk.type === "asset" && chunk.fileName.endsWith(".css")) {
          css =
            typeof chunk.source === "string"
              ? chunk.source
              : new TextDecoder().decode(chunk.source);
        }
      }
    }
  }

  if (!code) {
    throw new Error(`Failed to build ${label}: no JS output produced`);
  }

  return { code, css };
}

/**
 * Build a single renderer plugin as CJS with React externalized.
 *
 * @param pluginEntry Absolute path to the plugin entry file
 * @param pluginName Plugin name (used for output file name)
 * @returns Promise resolving to the built code and CSS
 */
export async function buildRendererPlugin(
  pluginEntry: string,
  pluginName: string,
): Promise<RendererPluginOutput> {
  const srcDir = getSrcDir();

  const result = await build({
    configFile: false,
    mode: "production",
    plugins: [tailwindcss()],
    esbuild: {
      jsx: "automatic",
      jsxImportSource: "react",
      jsxDev: false,
    },
    resolve: {
      alias: {
        "@/": `${srcDir}/`,
      },
    },
    build: {
      write: false,
      lib: {
        entry: pluginEntry,
        formats: ["cjs"],
        fileName: () => `${pluginName}.js`,
      },
      rolldownOptions: {
        external: ["react", "react/jsx-runtime"],
        output: {
          assetFileNames: `${pluginName}.[ext]`,
          codeSplitting: false,
        },
        onwarn(warning, warn) {
          if (
            warning.code === "MODULE_LEVEL_DIRECTIVE" &&
            warning.message?.includes('"use client"')
          ) {
            return;
          }
          warn(warning);
        },
      },
      minify: true,
      sourcemap: false,
    },
    define: {
      "process.env.NODE_ENV": JSON.stringify("production"),
    },
    logLevel: "warn",
  });

  const { code, css } = extractBuildOutput(result, `${pluginName} renderer plugin`);

  return { name: pluginName, code, css };
}

/**
 * Build all renderer plugins in parallel.
 *
 * @param plugins Optional array of plugin definitions (defaults to RENDERER_PLUGINS)
 * @returns Promise resolving to array of build outputs
 */
export async function buildAllRendererPlugins(
  plugins: RendererPluginDef[] = RENDERER_PLUGINS,
): Promise<RendererPluginOutput[]> {
  return Promise.all(plugins.map((plugin) => buildRendererPlugin(plugin.entry, plugin.name)));
}
