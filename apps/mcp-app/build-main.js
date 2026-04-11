/**
 * Build the MCP Apps widget as a single ESM bundle using Vite (Rollup).
 *
 * Produces dist/mcp-app.js (and optionally dist/mcp-app.css if Vite extracts CSS).
 * The HTML inlining step is handled by build-html.js which runs after this.
 */

import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { build } from "vite-plus";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

await build({
  configFile: false,
  root: __dirname,
  mode: "production",
  plugins: [tailwindcss()],
  esbuild: {
    jsx: "automatic",
    jsxImportSource: "react",
    jsxDev: false,
  },
  build: {
    write: true,
    outDir: "dist",
    emptyDirBefore: true,
    lib: {
      entry: path.resolve(__dirname, "src/mcp-app.tsx"),
      formats: ["es"],
      fileName: () => "mcp-app.js",
    },
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
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
