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

  // Wait for the handshake to complete with a non-zero cell count — the
  // daemon reads the file and returns NotebookConnectionInfo.cell_count.
  await expect
    .poll(
      async () => {
        const info = await window.evaluate(() => window.electronAPI?.info());
        return info?.cellCount ?? 0;
      },
      { timeout: 15_000, intervals: [200, 500, 1000] },
    )
    .toBeGreaterThan(0);

  // Cell materialization goes through WASM after initial sync. Wait for the
  // first rendered cell to show up in the DOM. `data-cell-id` is the stable
  // attribute the NotebookView uses for every cell.
  await window.waitForSelector("[data-cell-id]", { timeout: 20_000 });
  const cellCount = await window.locator("[data-cell-id]").count();
  process.stdout.write(`[test] visible cells in DOM: ${cellCount}\n`);
  expect(cellCount).toBeGreaterThan(0);

  await app.close();
});
