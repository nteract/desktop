import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { defineConfig } from "vite-plus";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@/": path.resolve(__dirname, "../../src") + "/",
      "~/": path.resolve(__dirname, "./src") + "/",
      "runtimed-wasm": path.resolve(__dirname, "../notebook/src/wasm/runtimed-wasm"),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    port: 5175,
    proxy: {
      "/api": "http://localhost:8743",
      "/ws": {
        target: "ws://localhost:8743",
        ws: true,
      },
    },
  },
});
