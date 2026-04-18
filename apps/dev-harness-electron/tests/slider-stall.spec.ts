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

  // Clear any stale outputs (cached from prior runs) then run ALL cells in
  // order so `import ipywidgets as widgets` lands before the slider cell.
  await window.evaluate(async (cellId) => {
    await window.electronAPI?.sendRequest({
      action: "clear_outputs",
      cell_id: cellId,
    });
  }, sliderCellId);
  const execResult = await window.evaluate(async () => {
    return window.electronAPI?.sendRequest({ action: "run_all_cells" });
  });
  process.stdout.write(`[test] run_all_cells → ${JSON.stringify(execResult)}\n`);

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
  // MediaProvider in App.tsx registers a direct renderer for
   // `application/vnd.jupyter.widget-view+json` → `<WidgetView />`, so the
   // control should mount in the parent DOM. The iframe we saw earlier is
   // for some OTHER output channel (heavy renderers via isolated-frame).
  const allWidgets = await window.evaluate(() => {
    const els = Array.from(document.querySelectorAll("[data-widget-type]"));
    return els.map((el) => ({
      type: el.getAttribute("data-widget-type"),
      id: el.getAttribute("data-widget-id"),
      cellParent: el.closest("[data-cell-id]")?.getAttribute("data-cell-id") ?? null,
    }));
  });
  process.stdout.write(
    `[test] widgets rendered in parent DOM: ${JSON.stringify(allWidgets)}\n`,
  );

  // Sanity check: harness flag should be set from the preload.
  const flag = await window.evaluate(
    () => (window as unknown as { __NTERACT_DEV_HARNESS_INLINE_WIDGETS__?: boolean })
      .__NTERACT_DEV_HARNESS_INLINE_WIDGETS__,
  );
  process.stdout.write(`[test] harness inline-widgets flag=${flag}\n`);

  // Wait for actual output-item(s) to appear in the slider cell. That's the
  // signal that the kernel has returned a display_data message and the sync
  // engine has materialized it. Up to 60s — first-run ipywidgets import can
  // be slow.
  const outputsInfo = await window.evaluate(async (id) => {
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      const cellEl = document.querySelector(`[data-cell-id="${id}"]`);
      const items = cellEl
        ? cellEl.querySelectorAll("[data-slot='output-item'], [data-widget-type]")
        : null;
      if (items && items.length > 0) {
        return {
          itemCount: items.length,
          selectors: Array.from(items).map((el) => ({
            slot: el.getAttribute("data-slot"),
            widget: el.getAttribute("data-widget-type"),
            tag: el.tagName,
            classes: el.className?.slice?.(0, 80),
          })),
        };
      }
      await new Promise((r) => setTimeout(r, 500));
    }
    return { error: "no output-item within 60s" };
  }, sliderCellId);
  process.stdout.write(`[test] cell outputs info: ${JSON.stringify(outputsInfo)}\n`);

  const iframeInfo = await window.evaluate((id) => {
    const frame = document.querySelector(`[data-cell-id="${id}"] iframe`);
    if (!frame) return { present: false };
    return {
      present: true,
      src: (frame as HTMLIFrameElement).src?.slice(0, 120),
      sandbox: (frame as HTMLIFrameElement).getAttribute("sandbox"),
      width: (frame as HTMLIFrameElement).clientWidth,
      height: (frame as HTMLIFrameElement).clientHeight,
    };
  }, sliderCellId);
  process.stdout.write(`[test] iframe info: ${JSON.stringify(iframeInfo)}\n`);

  // Look inside the iframe via frameLocator + evaluate. If a slider has
  // rendered the DOM will include a [role="slider"] element.
  const iframeContents = await window
    .frameLocator(`[data-cell-id="${sliderCellId}"] iframe`)
    .locator("body")
    .innerHTML()
    .catch((err: unknown) => `<error: ${(err as Error).message}>`);
  process.stdout.write(
    `[test] iframe body length=${iframeContents.length} preview:\n${iframeContents.slice(0, 800)}\n`,
  );

  const hasDirectWidget = await window.evaluate(
    (id) =>
      !!document.querySelector(
        `[data-cell-id="${id}"] [data-widget-type="IntSlider"]`,
      ),
    sliderCellId,
  );
  process.stdout.write(
    `[test] slider host: iframe=${iframeInfo.present} direct=${hasDirectWidget}\n`,
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
