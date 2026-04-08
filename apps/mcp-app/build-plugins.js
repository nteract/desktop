/**
 * Build renderer plugins as standalone CJS files for the daemon to serve.
 *
 * Outputs to crates/runt-mcp/assets/plugins/{name}.js (and {name}.css if CSS produced).
 *
 * Usage:
 *   node build-plugins.js
 *   # or via package.json script:
 *   npm run build:plugins
 */

import { build } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs/promises";
import { wrapForMcpApp } from "./src/lib/wrap-plugin.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../..");
const srcDir = path.resolve(repoRoot, "src");
const outDir = path.resolve(repoRoot, "crates/runt-mcp/assets/plugins");

const PLUGINS = [
  {
    name: "markdown",
    entry: path.resolve(srcDir, "isolated-renderer/markdown-renderer.tsx"),
  },
  {
    name: "plotly",
    entry: path.resolve(srcDir, "isolated-renderer/plotly-renderer.tsx"),
  },
  {
    name: "vega",
    entry: path.resolve(srcDir, "isolated-renderer/vega-renderer.tsx"),
  },
  {
    name: "leaflet",
    entry: path.resolve(srcDir, "isolated-renderer/leaflet-renderer.tsx"),
  },
];

async function buildRendererPlugin(pluginEntry, pluginName) {
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
      rollupOptions: {
        external: ["react", "react/jsx-runtime"],
        output: {
          inlineDynamicImports: true,
          assetFileNames: `${pluginName}.[ext]`,
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

  if (!code) {
    throw new Error(
      `Failed to build ${pluginName} renderer plugin: no JS output produced`,
    );
  }

  return { code, css };
}

async function main() {
  await fs.mkdir(outDir, { recursive: true });

  for (const { name, entry } of PLUGINS) {
    process.stdout.write(`Building ${name}...`);
    const { code, css } = await buildRendererPlugin(entry, name);

    const jsPath = path.join(outDir, `${name}.js`);
    const wrapped = wrapForMcpApp(code);
    await fs.writeFile(jsPath, wrapped, "utf8");
    const jsSizeKb = (wrapped.length / 1024).toFixed(1);

    let cssSizeKb = "0.0";
    if (css) {
      const cssPath = path.join(outDir, `${name}.css`);
      await fs.writeFile(cssPath, css, "utf8");
      cssSizeKb = (css.length / 1024).toFixed(1);
    }

    console.log(` done (JS: ${jsSizeKb} kB${css ? `, CSS: ${cssSizeKb} kB` : ""})`);
  }

  console.log(`\nPlugins written to ${outDir}`);
  console.log("Rebuild the daemon (cargo build -p runtimed) to embed them.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
