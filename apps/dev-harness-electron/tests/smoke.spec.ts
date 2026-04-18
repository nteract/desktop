import { _electron as electron, expect, test } from "@playwright/test";
import path from "node:path";

// Smoke test: launch the Electron harness and verify the main window loads.
//
// Prereqs (the test does NOT start them):
//   1. `runtimed` dev daemon running (e.g. `cargo xtask dev-daemon`)
//   2. Vite dev server running on $RUNTIMED_VITE_PORT or 5174
//        (e.g. `pnpm --filter notebook-ui dev`)
//
// The test launches a fresh Electron main process each run. Daemon socket is
// auto-discovered via `runt daemon status --json` (or RUNTIMED_SOCKET_PATH).

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");

test("electron harness opens a window and loads the notebook UI", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: {
      ...process.env,
      // Leave RUNTIMED_VITE_PORT/RUNTIMED_SOCKET_PATH/HARNESS_NOTEBOOK_ID to
      // the environment — this test just checks the window renders.
    },
  });

  const window = await app.firstWindow({ timeout: 15_000 });

  // Title is set in main/index.js BrowserWindow options.
  const title = await window.title();
  expect(title.length).toBeGreaterThan(0);

  // Wait for React to hydrate anything under #root.
  await window.waitForSelector("#root", { timeout: 15_000 });

  await app.close();
});
