import { defineConfig } from "vitest/config";
import path from "path";

export default defineConfig({
  test: {
    environment: "jsdom",
    include: [
      "src/**/__tests__/**/*.test.{ts,tsx}",
      "apps/notebook/src/**/__tests__/**/*.test.{ts,tsx}",
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
