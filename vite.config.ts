import { defineConfig } from "vite-plus";

const ignoreNonSource = [
  ".claude/**",
  ".codex/**",
  ".github/**",
  ".zed/**",
  "contributing/**",
  "scripts/**",
  "crates/**",
  "e2e/**",
  "python/**",
  "**/*.md",
  "**/*.yml",
  "**/*.yaml",
  "**/*.json",
  "**/*.toml",
  "**/wasm/**",
  "**/dist/**",
  "**/lib/**",
  "**/node_modules/**",
];

export default defineConfig({
  fmt: {
    ignorePatterns: ignoreNonSource,
  },
  lint: {
    ignorePatterns: ignoreNonSource,
  },
});
