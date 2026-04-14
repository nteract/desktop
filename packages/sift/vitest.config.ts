import path from "node:path";
import fs from "node:fs";
import { defineConfig } from "vite-plus";

// Use real WASM JS glue if built, otherwise fall back to mock
const realWasmGlue = path.resolve(__dirname, "../../crates/sift-wasm/pkg/nteract_predicate.js");
const mockWasmGlue = path.resolve(
  __dirname,
  "src/__mocks__/nteract-predicate/nteract_predicate.js",
);
const wasmGluePath = fs.existsSync(realWasmGlue) ? realWasmGlue : mockWasmGlue;

export default defineConfig({
  resolve: {
    alias: {
      "nteract-predicate/nteract_predicate.js": wasmGluePath,
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
