import { _electron as electron, expect, test } from "@playwright/test";
import path from "node:path";

// First-cut slider-stall reproduction scaffold.
//
// Goal: reach the point where we can drive an ipywidgets IntSlider and watch
// sync frames for the stall signature (sent_hashes growing while inbound
// halts). Currently best-effort — if kernel launch or iframe access fails,
// the test logs what it got so we can iterate.
//
// Prereqs:
//   1. `runtimed` dev daemon running
//   2. Vite dev server running on $RUNTIMED_VITE_PORT or 5174
//   3. The daemon's default python env must have `ipywidgets` available

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");
const IPYWIDGETS_DEMO = path.resolve(
  __dirname,
  "..",
  "..",
  "..",
  "notebooks",
  "ipywidgets-demo.ipynb",
);

test("drive an ipywidgets IntSlider and observe frame flow", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: { ...process.env, HARNESS_NOTEBOOK_PATH: IPYWIDGETS_DEMO },
  });

  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  const window = await app.firstWindow({ timeout: 15_000 });
  window.on("pageerror", (err) =>
    process.stderr.write(`[renderer pageerror] ${err.message}\n`),
  );

  // Wait for the file to stream in.
  await window.waitForSelector("[data-cell-id]", { timeout: 30_000 });
  const cellCount = await window.locator("[data-cell-id]").count();
  process.stdout.write(`[test] cells rendered: ${cellCount}\n`);

  // Launch the kernel. This goes through ElectronTransport.sendRequest →
  // main process → daemon (NotebookRequest::LaunchKernel).
  const launchResult = await window.evaluate(async () => {
    return window.electronAPI?.sendRequest({
      action: "launch_kernel",
      kernel_type: "python",
      env_source: "uv:inline",
      notebook_path: null,
    });
  });
  process.stdout.write(`[test] launch_kernel → ${JSON.stringify(launchResult)}\n`);

  // Find the IntSlider cell. It's the cell whose source includes
  // `widgets.IntSlider(` in the fixture.
  const sliderCellId = await window.evaluate(() => {
    const cells = Array.from(document.querySelectorAll("[data-cell-id]"));
    for (const el of cells) {
      const code = el.textContent ?? "";
      if (code.includes("IntSlider(")) return el.getAttribute("data-cell-id");
    }
    return null;
  });
  process.stdout.write(`[test] IntSlider cell_id=${sliderCellId}\n`);

  if (!sliderCellId) {
    test.skip(true, "no IntSlider cell in fixture — can't drive the stall");
    return;
  }

  // Execute the cell.
  const execResult = await window.evaluate(async (cellId) => {
    return window.electronAPI?.sendRequest({
      action: "execute_cell",
      cell_id: cellId,
    });
  }, sliderCellId);
  process.stdout.write(`[test] execute_cell → ${JSON.stringify(execResult)}\n`);

  // Wait for the output iframe to render. Output containers get a
  // `data-output-cell-id` attribute (or similar) — observe what actually
  // shows up and adjust.
  try {
    await window.waitForSelector(`[data-cell-id="${sliderCellId}"] iframe`, {
      timeout: 30_000,
    });
    const iframeCount = await window
      .locator(`[data-cell-id="${sliderCellId}"] iframe`)
      .count();
    process.stdout.write(`[test] output iframes for cell: ${iframeCount}\n`);
  } catch {
    const html = await window
      .locator(`[data-cell-id="${sliderCellId}"]`)
      .innerHTML()
      .catch(() => "<unavailable>");
    process.stdout.write(`[test] cell HTML after execute (first 1KB):\n${html.slice(0, 1024)}\n`);
    test.skip(true, "widget iframe never materialized — likely kernel/env issue");
    return;
  }

  // Drive keys into the slider range input and see frames keep flowing.
  const sliderFrame = window
    .frameLocator(`[data-cell-id="${sliderCellId}"] iframe`)
    .locator('input[type="range"]');
  await sliderFrame.first().focus();

  const start = Date.now();
  for (let i = 0; i < 200; i++) {
    await window.keyboard.press("ArrowRight");
  }
  const elapsed = Date.now() - start;
  process.stdout.write(`[test] 200 ArrowRight presses in ${elapsed}ms\n`);

  // No stall detector yet — this run just validates the plumbing. Follow-up
  // will wire up frame-trace counters from #1886 and assert on advancement.
  await app.close();
});
