import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { visualizer } from "rollup-plugin-visualizer";
import { defineConfig } from "vite";
import wasm from "vite-plugin-wasm";
import topLevelAwait from "vite-plugin-top-level-await";
import { isolatedRendererPlugin } from "./vite-plugin-isolated-renderer";

export default defineConfig({
  plugins: [
    wasm(),
    topLevelAwait(),
    react(),
    tailwindcss(),
    isolatedRendererPlugin(),
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
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, "index.html"),
        onboarding: path.resolve(__dirname, "onboarding/index.html"),
      },
      output: {
        entryFileNames: "assets/[name].js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name].[ext]",
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
});
