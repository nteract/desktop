import { defineConfig } from "vitest/config";
import path from "path";
import { rawLibPlugin } from "./apps/notebook/vite-plugin-raw-lib";

export default defineConfig({
  plugins: [rawLibPlugin(path.resolve(__dirname, "./node_modules"))],
  test: {
    environment: "jsdom",
    include: [
      "src/**/__tests__/**/*.test.{ts,tsx}",
      "apps/notebook/src/**/__tests__/**/*.test.{ts,tsx}",
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
