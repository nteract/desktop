import path from "node:path";
import fs from "node:fs";
import { defineConfig } from "vite-plus";

// Use real WASM JS glue if built, otherwise fall back to mock
const realWasmGlue = path.resolve(__dirname, "../../crates/sift-wasm/pkg/sift_wasm.js");
const mockWasmGlue = path.resolve(__dirname, "src/__mocks__/sift-wasm/sift_wasm.js");
const wasmGluePath = fs.existsSync(realWasmGlue) ? realWasmGlue : mockWasmGlue;

export default defineConfig({
  resolve: {
    alias: {
      "sift-wasm/sift_wasm.js": wasmGluePath,
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
