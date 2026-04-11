import { resolve } from "node:path";
import { defineConfig } from "vite-plus";

const wasmPkg = resolve(__dirname, "../../crates/nteract-predicate/pkg");

export default defineConfig({
  resolve: {
    alias: {
      "nteract-predicate": wasmPkg,
    },
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/setupTests.ts"],
    exclude: ["node_modules", "dist", "e2e/**", "tests/e2e/**", ".claude/**"],
    css: false,
  },
});
