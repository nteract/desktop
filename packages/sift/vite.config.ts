import { defineConfig } from "vite-plus";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";

const wasmPkg = resolve(__dirname, "../../crates/sift-wasm/pkg");

export default defineConfig({
  base: "/",
  plugins: [tailwindcss()],
  resolve: {
    alias: {
      "sift-wasm": wasmPkg,
    },
  },
  build: {
    rolldownOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        notebook: resolve(__dirname, "notebook.html"),
      },
    },
  },
});
