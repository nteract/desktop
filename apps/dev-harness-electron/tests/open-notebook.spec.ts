import { _electron as electron, expect, test } from "@playwright/test";
import path from "node:path";

// Open an on-disk notebook file through the `open_notebook` handshake and
// verify cells render. Exercises the cell-materialization path the
// create_notebook smoke test (empty notebook, cell_count=0) does not.
//
// Prereqs:
//   1. `runtimed` dev daemon running
//   2. Vite dev server running on $RUNTIMED_VITE_PORT or 5174
//
// Uses notebooks/ipywidgets-demo.ipynb from the repo root.

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");
const IPYWIDGETS_DEMO = path.resolve(
  __dirname,
  "..",
  "..",
  "..",
  "notebooks",
  "ipywidgets-demo.ipynb",
);

test("open ipywidgets-demo.ipynb and render cells", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: {
      ...process.env,
      HARNESS_NOTEBOOK_PATH: IPYWIDGETS_DEMO,
    },
  });

  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  const window = await app.firstWindow({ timeout: 15_000 });
  window.on("pageerror", (err) => {
    process.stderr.write(`[renderer pageerror] ${err.message}\n`);
  });

  await window.waitForSelector("#root", { timeout: 15_000 });

  // Daemon's cell_count in NotebookConnectionInfo is 0 at handshake time —
  // the file is stream-loaded only once the client starts Automerge sync
  // (crates/runtimed/src/daemon.rs:1773-1780). So don't assert on cached
  // info; wait for cells to materialize in the DOM instead.
  await window.waitForSelector("[data-cell-id]", { timeout: 30_000 });
  const cellCount = await window.locator("[data-cell-id]").count();
  process.stdout.write(`[test] visible cells in DOM: ${cellCount}\n`);
  expect(cellCount).toBeGreaterThan(0);

  await app.close();
});
