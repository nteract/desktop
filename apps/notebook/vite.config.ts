import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { visualizer } from "rollup-plugin-visualizer";
import { defineConfig } from "vite";
import { isolatedRendererPlugin } from "./vite-plugin-isolated-renderer";

export default defineConfig(({ command }) => {
  const debugBundleSourceMapsEnabled =
    process.env.RUNT_NOTEBOOK_DEBUG_BUILD === "1";
  const isolatedRendererSourceMapsEnabled =
    command === "serve" || debugBundleSourceMapsEnabled;
  const reactDevToolsBootstrap = {
    name: "react-devtools-bootstrap",
    transformIndexHtml(html: string) {
      if (command !== "serve") {
        return html;
      }

      return {
        html,
        tags: [
          {
            tag: "script",
            attrs: {
              type: "module",
              src: "/src/lib/connect-react-devtools.ts",
            },
            injectTo: "head-prepend",
          },
        ],
      };
    },
  };

  return {
    plugins: [
      reactDevToolsBootstrap,
      react(),
      tailwindcss(),
      isolatedRendererPlugin({
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
      rollupOptions: {
        input: {
          main: path.resolve(__dirname, "index.html"),
          onboarding: path.resolve(__dirname, "onboarding/index.html"),
          upgrade: path.resolve(__dirname, "upgrade/index.html"),
          settings: path.resolve(__dirname, "settings/index.html"),
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
  };
});
