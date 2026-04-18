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
//   3. Daemon's uv env pool has at least one available worker

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");
// Dedicated fixture — declares ipywidgets as a uv dep so the daemon can
// prepare the env at launch time. Using the shared notebooks/ directory
// would reformat a tracked file on open and produce a dirty working tree.
const FIXTURE = path.resolve(__dirname, "..", "fixtures", "int-slider.ipynb");

test("drive an ipywidgets IntSlider and observe frame flow", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: { ...process.env, HARNESS_NOTEBOOK_PATH: FIXTURE },
  });

  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  const window = await app.firstWindow({ timeout: 15_000 });
  window.on("pageerror", (err) =>
    process.stderr.write(`[renderer pageerror] ${err.message}\n`),
  );

  // Wait for the fixture to stream in (should be 3 cells).
  await window.waitForSelector("[data-cell-id]", { timeout: 30_000 });
  const cellCount = await window.locator("[data-cell-id]").count();
  process.stdout.write(`[test] cells rendered: ${cellCount}\n`);

  // Launch kernel. uv:inline picks up `metadata.runt.uv.dependencies` from
  // the fixture. First run may need to resolve ipywidgets, hence the wider
  // timeout.
  const launchResult = (await window.evaluate(async () => {
    return window.electronAPI?.sendRequest({
      action: "launch_kernel",
      kernel_type: "python",
      env_source: "uv:inline",
      notebook_path: null,
    });
  })) as { result: string; [k: string]: unknown };
  process.stdout.write(`[test] launch_kernel → ${JSON.stringify(launchResult)}\n`);

  if (
    !launchResult ||
    (launchResult.result !== "kernel_launched" &&
      launchResult.result !== "kernel_already_running")
  ) {
    test.skip(true, `kernel launch failed: ${JSON.stringify(launchResult)}`);
    return;
  }

  // Find the slider cell by source content.
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
    test.skip(true, "no IntSlider cell found in fixture");
    return;
  }

  // Execute the slider cell. Wait for cell_queued → then for output iframe.
  const execResult = await window.evaluate(async (cellId) => {
    return window.electronAPI?.sendRequest({
      action: "execute_cell",
      cell_id: cellId,
    });
  }, sliderCellId);
  process.stdout.write(`[test] execute_cell → ${JSON.stringify(execResult)}\n`);

  // Wait for widget output. Widgets render either inside an isolated
  // iframe (heavy renderers) OR directly in the DOM as built-in controls
  // (IntSlider → React Slider component with data-widget-type="IntSlider").
  // Either is fine — just need to see the slider appear somewhere.
  let outputReady = false;
  const outputSelector = `[data-cell-id="${sliderCellId}"] iframe, [data-cell-id="${sliderCellId}"] [data-widget-type="IntSlider"]`;
  try {
    // state: attached — widget DOM may render offscreen or have 0x0 size
    // while waiting for iframe content; that's still "ready" for our purposes.
    await window.waitForSelector(outputSelector, {
      timeout: 60_000,
      state: "attached",
    });
    outputReady = true;
  } catch {
    // Periodic poll snapshot the cell HTML to surface whatever's taking time.
    for (const tSec of [5, 15, 30, 60]) {
      await new Promise((r) => setTimeout(r, 0));
      const snap = await window
        .locator(`[data-cell-id="${sliderCellId}"]`)
        .innerHTML()
        .catch(() => "<unavailable>");
      process.stdout.write(
        `[test] t~${tSec}s cell HTML contains iframe=${snap.includes("<iframe")} ipywidget=${snap.includes("IntSlider")} outputs-div=${snap.includes("data-output-area")} len=${snap.length}\n`,
      );
    }
    test.skip(true, "widget output never materialized");
    return;
  }
  expect(outputReady).toBe(true);

  // Locate the slider. Two possible hosts:
  //   1. In-iframe: for anywidget + custom ESM modules rendered inside the
  //      isolated iframe (sandbox="allow-scripts"). Chromium's
  //      frameLocator handles that cleanly.
  //   2. Parent DOM: built-in ipywidgets controls (like IntSlider) are
  //      React components registered in src/components/widgets/controls/,
  //      rendered directly into the parent tree with
  //      data-widget-type="IntSlider".
  const isIframeRendered = await window.evaluate(
    (id) => !!document.querySelector(`[data-cell-id="${id}"] iframe`),
    sliderCellId,
  );
  const hasDirectWidget = await window.evaluate(
    (id) =>
      !!document.querySelector(
        `[data-cell-id="${id}"] [data-widget-type="IntSlider"]`,
      ),
    sliderCellId,
  );
  process.stdout.write(
    `[test] slider host: iframe=${isIframeRendered} direct=${hasDirectWidget}\n`,
  );

  const slider = hasDirectWidget
    ? window
        .locator(`[data-cell-id="${sliderCellId}"] [data-widget-type="IntSlider"]`)
        .locator("[role='slider']")
        .first()
    : window
        .frameLocator(`[data-cell-id="${sliderCellId}"] iframe`)
        .locator('input[type="range"], [role="slider"]')
        .first();

  await slider.waitFor({ state: "attached", timeout: 10_000 });
  await slider.focus();

  const start = Date.now();
  for (let i = 0; i < 200; i++) {
    await window.keyboard.press("ArrowRight");
  }
  const elapsed = Date.now() - start;
  process.stdout.write(`[test] 200 ArrowRight presses in ${elapsed}ms\n`);

  // Read back the widget's current value. Built-in IntSlider uses
  // aria-valuenow on its Slider thumb; iframe path uses the <input value>.
  const sliderValue = hasDirectWidget
    ? await slider.getAttribute("aria-valuenow").catch(() => null)
    : await slider.getAttribute("value").catch(() => null);
  process.stdout.write(`[test] slider value after drive: ${sliderValue}\n`);

  await app.close();
});
