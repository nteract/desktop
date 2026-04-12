import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite-plus";

export default defineConfig(({ command }) => {
  if (command === "serve") {
    return {
      root: "src",
      plugins: [tailwindcss()],
      server: {
        open: "/dev/index.html",
      },
    };
  }

  return {
    plugins: [tailwindcss()],
    esbuild: {
      jsx: "automatic",
      jsxImportSource: "react",
      jsxDev: false,
    },
    build: {
      outDir: "dist",
      emptyDirBefore: true,
      lib: {
        entry: "src/mcp-app.tsx",
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
    run: {
      tasks: {
        build: {
          command: "vp build && node build-html.js",
        },
        "build:plugins": {
          command: "node build-plugins.ts",
        },
        "build:all": {
          command: "echo 'MCP app build complete'",
          dependsOn: ["build", "build:plugins"],
        },
      },
    },
    define: {
      "process.env.NODE_ENV": JSON.stringify("production"),
    },
    logLevel: "warn",
  };
});
