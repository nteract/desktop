/**
 * Vite Plugin: Isolated Renderer
 *
 * Builds the isolated renderer bundle during the notebook build and exposes
 * it as a virtual module. This eliminates the need for a separate build step.
 *
 * Usage:
 *   import { rendererCode, rendererCss } from 'virtual:isolated-renderer';
 */

import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { build, type Plugin } from "vite";

const VIRTUAL_MODULE_ID = "virtual:isolated-renderer";
const RESOLVED_VIRTUAL_MODULE_ID = `\0${VIRTUAL_MODULE_ID}`;

interface IsolatedRendererPluginOptions {
  /**
   * Path to the isolated renderer entry file.
   * @default "../../src/isolated-renderer/index.tsx"
   */
  entry?: string;
  /**
   * Enable minification for production builds.
   * @default false
   */
  minify?: boolean;
  /**
   * Source map mode for the embedded renderer bundle.
   * Use inline source maps when the bundle is in-memory or injected.
   * @default false
   */
  sourcemap?: false | "inline";
}

export function isolatedRendererPlugin(
  options: IsolatedRendererPluginOptions = {},
): Plugin {
  const {
    entry = path.resolve(__dirname, "../../src/isolated-renderer/index.tsx"),
    minify = false,
    sourcemap = false,
  } = options;

  let rendererCode = "";
  let rendererCss = "";
  let buildPromise: Promise<void> | null = null;

  // Directories to watch for changes that should trigger rebuild
  const isolatedRendererDir = path.resolve(
    __dirname,
    "../../src/isolated-renderer",
  );
  const componentsDir = path.resolve(__dirname, "../../src/components");

  function invalidateCache() {
    buildPromise = null;
    rendererCode = "";
    rendererCss = "";
  }

  async function buildRenderer() {
    const srcDir = path.resolve(__dirname, "../../src");

    const result = await build({
      configFile: false,
      // Force production mode to ensure esbuild uses jsx-runtime (not jsx-dev-runtime)
      mode: "production",
      plugins: [
        // Don't use React plugin - use esbuild's native JSX handling instead
        // The React plugin uses Babel which doesn't respect mode for JSX transform
        tailwindcss(),
        // Resolve vega-raw/vega-lite-raw/vega-embed-raw virtual modules.
        // These bypass restrictive "exports" fields in vega packages (v6+).
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
        // Use esbuild's native JSX handling with automatic runtime
        // This properly bundles jsx-runtime into the IIFE
        jsx: "automatic",
        jsxImportSource: "react",
        // CRITICAL: Explicitly disable jsxDev to use production runtime
        // Without this, Vite's dev server passes jsxDev: true to esbuild,
        // which generates jsxDEV calls that fail in the sandboxed iframe
        jsxDev: false,
      },
      resolve: {
        alias: {
          "@/": `${srcDir}/`,
        },
      },
      build: {
        write: false, // Don't write to disk, return in memory
        lib: {
          entry,
          name: "IsolatedRenderer",
          formats: ["iife"],
          fileName: () => "isolated-renderer.js",
        },
        rollupOptions: {
          output: {
            inlineDynamicImports: true,
            assetFileNames: "isolated-renderer.[ext]",
          },
          external: [
            "@tauri-apps/api",
            "@tauri-apps/plugin-shell",
            "@tauri-apps/plugin-fs",
            /^@tauri-apps\/.*/,
          ],
          // Suppress "use client" directive warnings from node_modules
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
        minify,
        sourcemap,
      },
      define: {
        "process.env.NODE_ENV": JSON.stringify("production"),
      },
      logLevel: "warn", // Reduce noise during build
    });

    // Extract JS and CSS from build output
    const outputs = Array.isArray(result) ? result : [result];
    for (const output of outputs) {
      if ("output" in output) {
        for (const chunk of output.output) {
          if (chunk.type === "chunk" && chunk.fileName.endsWith(".js")) {
            rendererCode = chunk.code;
          } else if (
            chunk.type === "asset" &&
            chunk.fileName.endsWith(".css")
          ) {
            rendererCss =
              typeof chunk.source === "string"
                ? chunk.source
                : new TextDecoder().decode(chunk.source);
          }
        }
      }
    }

    if (!rendererCode) {
      throw new Error(
        "Failed to build isolated renderer: no JS output produced",
      );
    }
  }

  return {
    name: "isolated-renderer",

    async buildStart() {
      // Build the isolated renderer at the start of the main build
      // Cache the promise so we only build once even if called multiple times
      if (!buildPromise) {
        buildPromise = buildRenderer();
      }
      await buildPromise;
    },

    resolveId(id) {
      if (id === VIRTUAL_MODULE_ID) {
        return RESOLVED_VIRTUAL_MODULE_ID;
      }
    },

    async load(id) {
      if (id === RESOLVED_VIRTUAL_MODULE_ID) {
        // Ensure build is complete before returning module content
        // This handles race conditions in dev mode where load() may be
        // called before buildStart() completes
        if (buildPromise) {
          await buildPromise;
        }
        // Export the built code as strings
        return `
export const rendererCode = ${JSON.stringify(rendererCode)};
export const rendererCss = ${JSON.stringify(rendererCss)};
`;
      }
    },

    // For dev server: serve the virtual module
    configureServer(devServer) {
      // Ensure renderer is built before serving
      devServer.middlewares.use(async (_req, _res, next) => {
        if (!buildPromise) {
          buildPromise = buildRenderer();
        }
        await buildPromise;
        next();
      });
    },

    // Handle HMR: rebuild when isolated renderer source files change
    async handleHotUpdate({ file, server: devServer }) {
      // Check if the changed file is part of the isolated renderer bundle
      const isIsolatedRendererFile =
        file.startsWith(isolatedRendererDir) ||
        // Components used by the isolated renderer
        (file.startsWith(componentsDir) &&
          (file.includes("/outputs/") ||
            file.includes("/isolated/") ||
            file.includes("/widgets/")));

      if (isIsolatedRendererFile) {
        console.log(
          `[isolated-renderer] Rebuilding due to change in: ${path.relative(path.resolve(__dirname, "../.."), file)}`,
        );
        invalidateCache();
        buildPromise = buildRenderer();
        await buildPromise;

        // Invalidate the virtual module to trigger re-import
        const mod = devServer.moduleGraph.getModuleById(
          RESOLVED_VIRTUAL_MODULE_ID,
        );
        if (mod) {
          devServer.moduleGraph.invalidateModule(mod);
          // Send HMR update
          devServer.ws.send({
            type: "full-reload",
            path: "*",
          });
        }
      }
    },
  };
}

export default isolatedRendererPlugin;
