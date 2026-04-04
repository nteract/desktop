import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { visualizer } from "rollup-plugin-visualizer";
import { defineConfig } from "vite";
import { isolatedRendererPlugin } from "./vite-plugin-isolated-renderer";
import { rawLibPlugin } from "./vite-plugin-raw-lib";

export default defineConfig(({ command }) => {
  const debugBundleSourceMapsEnabled =
    process.env.RUNT_NOTEBOOK_DEBUG_BUILD === "1";
  const isolatedRendererSourceMapsEnabled =
    command === "serve" || debugBundleSourceMapsEnabled;

  return {
    plugins: [
      react(),
      tailwindcss(),
      rawLibPlugin(path.resolve(__dirname, "../../node_modules")),
      isolatedRendererPlugin({
        minify: command !== "serve",
        sourcemap: isolatedRendererSourceMapsEnabled ? "inline" : false,
      }),
      visualizer({
        filename: "dist/stats.html",
        open: false,
        gzipSize: true,
        brotliSize: true,
      }),
    ],
    resolve: {
      alias: {
        "@/": path.resolve(__dirname, "../../src") + "/",
        "~/": path.resolve(__dirname, "./src") + "/",
      },
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
      sourcemap: debugBundleSourceMapsEnabled,
      chunkSizeWarningLimit: 10000,
      rollupOptions: {
        input: {
          main: path.resolve(__dirname, "index.html"),
          onboarding: path.resolve(__dirname, "onboarding/index.html"),
          upgrade: path.resolve(__dirname, "upgrade/index.html"),
          settings: path.resolve(__dirname, "settings/index.html"),
          feedback: path.resolve(__dirname, "feedback/index.html"),
        },
        output: {
          entryFileNames: "assets/[name]-[hash].js",
          chunkFileNames: "assets/[name]-[hash].js",
          assetFileNames: "assets/[name]-[hash].[ext]",
        },
      },
    },
    server: {
      port: parseInt(
        process.env.RUNTIMED_VITE_PORT || process.env.CONDUCTOR_PORT || "5174",
      ),
      strictPort: true,
    },
    base: "/",
  };
});
