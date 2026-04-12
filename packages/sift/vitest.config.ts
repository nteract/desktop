import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { defineConfig } from "vite-plus";

const wasmPkg = resolve(__dirname, "../../crates/nteract-predicate/pkg");
const wasmAvailable = existsSync(resolve(wasmPkg, "nteract_predicate.js"));

export default defineConfig({
  resolve: {
    alias: {
      "nteract-predicate": wasmAvailable
        ? wasmPkg
        : resolve(__dirname, "src/__mocks__/nteract-predicate"),
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
