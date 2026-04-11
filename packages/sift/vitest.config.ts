import { defineConfig } from "vite-plus";

export default defineConfig({
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/setupTests.ts"],
    exclude: ["node_modules", "dist", "e2e/**", "tests/e2e/**", ".claude/**"],
    css: false,
  },
});
