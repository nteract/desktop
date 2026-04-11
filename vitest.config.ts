import { defineConfig, type Plugin } from "vite-plus";
import path from "path";
import { rawLibPlugin } from "./apps/notebook/vite-plugin-raw-lib";

/** Stub virtual:renderer-plugin/* modules for tests (real builds use the Vite plugin). */
function rendererPluginStub(): Plugin {
  const prefix = "virtual:renderer-plugin/";
  return {
    name: "renderer-plugin-stub",
    resolveId(id) {
      if (id.startsWith(prefix)) return `\0${id}`;
    },
    load(id) {
      if (id.startsWith(`\0${prefix}`)) {
        return 'export const code = ""; export const css = "";';
      }
    },
  };
}

export default defineConfig({
  plugins: [rawLibPlugin(path.resolve(__dirname, "./node_modules")), rendererPluginStub()],
  test: {
    environment: "jsdom",
    include: [
      "src/**/__tests__/**/*.test.{ts,tsx}",
      "apps/notebook/src/**/__tests__/**/*.test.{ts,tsx}",
      "apps/mcp-app/src/**/__tests__/**/*.test.{js,ts,tsx}",
      "packages/**/tests/**/*.test.{ts,tsx}",
    ],
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
