/**
 * Vite Plugin: Isolated Renderer
 *
 * Loads pre-built renderer plugin artifacts from disk and exposes them as
 * virtual modules. The artifacts live under `apps/notebook/src/renderer-plugins/`
 * (gitignored) and are produced by `cargo xtask wasm`, which chains into
 * the renderer-plugin builder after rebuilding sift-wasm.
 *
 * In dev mode (Vite dev server), changes to isolated renderer source files
 * trigger a live rebuild + HMR reload so you don't need to re-run the
 * xtask command during active renderer development.
 *
 * Usage:
 *   import { rendererCode, rendererCss } from 'virtual:isolated-renderer';
 *   import { code, css } from 'virtual:renderer-plugin/plotly';
 */

import fs from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { build, type Plugin } from "vite-plus";
import {
  buildAllRendererPlugins,
  type RendererPluginOutput,
} from "../../src/build/renderer-plugin-builder";

const VIRTUAL_MODULE_ID = "virtual:isolated-renderer";
const RESOLVED_VIRTUAL_MODULE_ID = `\0${VIRTUAL_MODULE_ID}`;

// Renderer plugins get their own virtual modules so Vite can code-split them.
// Without this, importing the core IIFE would also pull in all plugin strings.
const VIRTUAL_PLUGIN_PREFIX = "virtual:renderer-plugin/";
const RESOLVED_PLUGIN_PREFIX = "\0virtual:renderer-plugin/";

/** Directory containing pre-built renderer plugin artifacts (gitignored, built by `cargo xtask wasm`). */
const PREBUILT_DIR = path.resolve(__dirname, "../notebook/src/renderer-plugins");

/** Plugin names that have pre-built artifacts. */
const PLUGIN_NAMES = ["markdown", "plotly", "vega", "leaflet", "sift"];

interface IsolatedRendererPluginOptions {
  /**
   * Path to the isolated renderer entry file (used only for dev-mode rebuilds).
   * @default "../../src/isolated-renderer/index.tsx"
   */
  entry?: string;
  /**
   * Source map mode for dev-mode rebuilds.
   * @default false
   */
  sourcemap?: false | "inline";
}

/**
 * Read a pre-built artifact from disk, returning empty string if missing.
 */
function readPrebuilt(filename: string): string {
  const filepath = path.join(PREBUILT_DIR, filename);
  try {
    return fs.readFileSync(filepath, "utf-8");
  } catch {
    return "";
  }
}

export function isolatedRendererPlugin(options: IsolatedRendererPluginOptions = {}): Plugin {
  const {
    entry = path.resolve(__dirname, "../../src/isolated-renderer/index.tsx"),
    sourcemap = false,
  } = options;

  // In-memory cache for dev-mode rebuilds (overrides pre-built artifacts)
  let devRendererCode = "";
  let devRendererCss = "";
  let devPluginOutputs = new Map<string, RendererPluginOutput>();
  let devBuildPromise: Promise<void> | null = null;
  let isDevMode = false;

  // Directories to watch for changes that should trigger rebuild
  const isolatedRendererDir = path.resolve(__dirname, "../../src/isolated-renderer");
  const componentsDir = path.resolve(__dirname, "../../src/components");

  function invalidateDevCache() {
    devBuildPromise = null;
    devRendererCode = "";
    devRendererCss = "";
    devPluginOutputs = new Map();
  }

  async function buildRendererFromSource() {
    const srcDir = path.resolve(__dirname, "../../src");

    const result = await build({
      configFile: false,
      mode: "production",
      plugins: [
        tailwindcss(),
        {
          name: "vega-raw-resolve",
          resolveId(source: string) {
            const nodeModules = path.resolve(__dirname, "../../node_modules");
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
          entry,
          name: "IsolatedRenderer",
          formats: ["iife"],
          fileName: () => "isolated-renderer.js",
        },
        rolldownOptions: {
          output: {
            assetFileNames: "isolated-renderer.[ext]",
          },
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
            ) {
              return;
            }
            warn(warning);
          },
        },
        minify: false, // Dev-mode rebuilds skip minification for faster HMR
        sourcemap,
      },
      define: {
        "process.env.NODE_ENV": JSON.stringify("production"),
      },
      logLevel: "warn",
    });

    const outputs = Array.isArray(result) ? result : [result];
    for (const output of outputs) {
      if ("output" in output) {
        for (const chunk of output.output) {
          if (chunk.type === "chunk" && chunk.fileName.endsWith(".js")) {
            devRendererCode = chunk.code;
          } else if (chunk.type === "asset" && chunk.fileName.endsWith(".css")) {
            devRendererCss =
              typeof chunk.source === "string"
                ? chunk.source
                : new TextDecoder().decode(chunk.source);
          }
        }
      }
    }

    if (!devRendererCode) {
      throw new Error("Failed to build isolated renderer: no JS output produced");
    }

    const plugins = await buildAllRendererPlugins();
    for (const plugin of plugins) {
      devPluginOutputs.set(plugin.name, plugin);
    }
  }

  return {
    name: "isolated-renderer",

    async buildStart() {
      // Production builds: verify all pre-built artifacts exist
      if (!isDevMode) {
        const missing = ["isolated-renderer.js", ...PLUGIN_NAMES.map((n) => `${n}.js`)].filter(
          (f) => !readPrebuilt(f),
        );
        if (missing.length > 0) {
          throw new Error(
            `Pre-built renderer plugins missing: ${missing.join(", ")}\n` +
              "Run `cargo xtask renderer-plugins` to build them.",
          );
        }
      }
    },

    resolveId(id) {
      if (id === VIRTUAL_MODULE_ID) {
        return RESOLVED_VIRTUAL_MODULE_ID;
      }
      if (id.startsWith(VIRTUAL_PLUGIN_PREFIX)) {
        return `${RESOLVED_PLUGIN_PREFIX}${id.slice(VIRTUAL_PLUGIN_PREFIX.length)}`;
      }
    },

    async load(id) {
      // In dev mode, wait for any in-progress build
      if (isDevMode && devBuildPromise) {
        await devBuildPromise;
      }

      // Core IIFE bundle
      if (id === RESOLVED_VIRTUAL_MODULE_ID) {
        const code =
          isDevMode && devRendererCode ? devRendererCode : readPrebuilt("isolated-renderer.js");
        const css =
          isDevMode && devRendererCss ? devRendererCss : readPrebuilt("isolated-renderer.css");
        return `
export const rendererCode = ${JSON.stringify(code)};
export const rendererCss = ${JSON.stringify(css)};
`;
      }

      // Renderer plugin modules
      const pluginName = id.startsWith(RESOLVED_PLUGIN_PREFIX)
        ? id.slice(RESOLVED_PLUGIN_PREFIX.length)
        : null;
      if (pluginName) {
        // Dev mode: use freshly built output if available
        if (isDevMode) {
          const devPlugin = devPluginOutputs.get(pluginName);
          if (devPlugin) {
            return `
export const code = ${JSON.stringify(devPlugin.code)};
export const css = ${JSON.stringify(devPlugin.css)};
`;
          }
        }
        // Production: read from pre-built artifacts
        const code = readPrebuilt(`${pluginName}.js`);
        const css = readPrebuilt(`${pluginName}.css`);
        if (code) {
          return `
export const code = ${JSON.stringify(code)};
export const css = ${JSON.stringify(css)};
`;
        }
      }
    },

    // Dev server: build from source for live development
    configureServer(devServer) {
      isDevMode = true;
      devServer.middlewares.use(async (_req, _res, next) => {
        if (!devBuildPromise) {
          devBuildPromise = buildRendererFromSource();
        }
        await devBuildPromise;
        next();
      });
    },

    // HMR: rebuild from source when isolated renderer files change
    async handleHotUpdate({ file, server: devServer }) {
      const isIsolatedRendererFile =
        file.startsWith(isolatedRendererDir) ||
        (file.startsWith(componentsDir) &&
          (file.includes("/outputs/") ||
            file.includes("/isolated/") ||
            file.includes("/widgets/")));

      if (isIsolatedRendererFile) {
        console.log(
          `[isolated-renderer] Rebuilding due to change in: ${path.relative(path.resolve(__dirname, "../.."), file)}`,
        );
        invalidateDevCache();
        devBuildPromise = buildRendererFromSource();
        await devBuildPromise;

        const mod = devServer.moduleGraph.getModuleById(RESOLVED_VIRTUAL_MODULE_ID);
        if (mod) {
          devServer.moduleGraph.invalidateModule(mod);
        }

        for (const name of devPluginOutputs.keys()) {
          const pluginMod = devServer.moduleGraph.getModuleById(`${RESOLVED_PLUGIN_PREFIX}${name}`);
          if (pluginMod) {
            devServer.moduleGraph.invalidateModule(pluginMod);
          }
        }

        devServer.ws.send({
          type: "full-reload",
          path: "*",
        });
      }
    },
  };
}

export default isolatedRendererPlugin;
