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
    rolldownOptions: {
      output: {
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
