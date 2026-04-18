import { defineConfig } from "@playwright/test";

// Electron harness Playwright config.
// The tests launch the Electron main process via `_electron.launch()` —
// no browser install is needed for the "electron" project.
export default defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  use: {
    trace: "retain-on-failure",
  },
});
