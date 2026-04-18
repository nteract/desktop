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
    env: { ...process.env },
  });

  // Mirror main-process stdout/stderr to the test log.
  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  const window = await app.firstWindow({ timeout: 15_000 });
  window.on("console", (msg) => {
    process.stdout.write(`[renderer ${msg.type()}] ${msg.text()}\n`);
  });
  window.on("pageerror", (err) => {
    process.stderr.write(`[renderer pageerror] ${err.message}\n`);
  });

  const url = window.url();
  const title = await window.title();
  process.stdout.write(`[test] window url=${url} title=${title}\n`);

  expect(url).toMatch(/localhost:\d+/);

  try {
    await window.waitForSelector("#root", { timeout: 15_000 });
  } catch (e) {
    const html = await window.content().catch(() => "<unavailable>");
    process.stderr.write(`[test] waitForSelector(#root) timed out. HTML head:\n`);
    process.stderr.write(html.slice(0, 2000));
    process.stderr.write("\n");
    throw e;
  }

  await app.close();
});
