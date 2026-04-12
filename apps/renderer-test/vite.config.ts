import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { defineConfig } from "vite-plus";
import { isolatedRendererPlugin } from "../notebook/vite-plugin-isolated-renderer";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    isolatedRendererPlugin({ minify: false }),
  ],
  resolve: {
    alias: {
      "@/": path.resolve(__dirname, "../../src") + "/",
    },
  },
  server: {
    port: 5176,
    strictPort: true,
  },
});
