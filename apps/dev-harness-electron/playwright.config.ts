import { defineConfig } from "@playwright/test";

// Electron harness Playwright config.
// The tests launch the Electron main process via `_electron.launch()` —
// no browser install is needed for the "electron" project.
export default defineConfig({
  testDir: "./tests",
  // Widget tests exercise a real kernel launch + ipywidgets import, so the
  // first iteration in a cold env can take 60-90s just to resolve. Keep the
  // per-test timeout generous; individual waits in-test use tighter bounds.
  timeout: 180_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  use: {
    trace: "retain-on-failure",
  },
});
