/**
 * E2E Test: Widget Slider Stall Reproducer
 *
 * Reproduces an intermittent bug where rapid slider input into an
 * ipywidget (FloatSlider via @interact) freezes widget-state sync.
 * The frontend stops seeing state updates even though the kernel
 * keeps executing.
 *
 * The bug is triggered most reliably by keyboard arrow keys on a
 * focused slider — left/right arrows stepping the value rapidly.
 *
 * Strategy:
 * 1. Execute a cell that creates a FloatSlider via @interact
 * 2. Wait for the widget to render (iframe in output area)
 * 3. Click the iframe to focus it, then hammer arrow keys
 * 4. After rapid input, execute a second cell to verify the kernel
 *    is responsive
 * 5. The stall manifests as:
 *    a. The verification cell output never appears (kernel/sync stuck)
 *
 * Note: Widgets render inside a security-isolated iframe. We cannot
 * query [data-widget-type] or [role="slider"] from the parent window.
 * Instead we detect the iframe's presence and send keyboard events
 * after clicking into the iframe to focus it.
 *
 * Fixture: 16-widget-slider.ipynb
 *   Cell 0: @interact FloatSlider (print-based, no matplotlib)
 *   Cell 1: random.random() — lightweight kernel-responsiveness check
 */

import { browser } from "@wdio/globals";
import {
  getKernelStatus,
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

/**
 * Find the isolated iframe inside a cell's output area.
 * Returns the iframe element or null if not found.
 */
async function findWidgetIframe(cell) {
  try {
    const iframe = await cell.$('[data-slot="isolated-frame"]');
    if (await iframe.isExisting()) {
      return iframe;
    }
  } catch {
    // Element not found
  }
  return null;
}

/**
 * Send rapid alternating arrow keys. After clicking into the iframe
 * to focus the slider, these keyDown/keyUp events will be dispatched
 * to the focused element (the Radix slider thumb).
 */
async function rapidArrowKeys(presses = 100, delayMs = 0) {
  const actions = [];

  for (let i = 0; i < presses; i++) {
    const key = i % 2 === 0 ? "ArrowRight" : "ArrowLeft";
    actions.push({ type: "keyDown", value: key });
    actions.push({ type: "keyUp", value: key });
    if (delayMs > 0) {
      actions.push({ type: "pause", duration: delayMs });
    }
  }

  await browser.performActions([
    {
      type: "key",
      id: "arrow-keys",
      actions,
    },
  ]);
  await browser.releaseActions();
}

/**
 * Sustained unidirectional arrow-key barrage.
 */
async function sustainedArrowKeys(direction, presses = 50) {
  const actions = [];
  for (let i = 0; i < presses; i++) {
    actions.push({ type: "keyDown", value: direction });
    actions.push({ type: "keyUp", value: direction });
  }

  await browser.performActions([
    {
      type: "key",
      id: "sustained-arrows",
      actions,
    },
  ]);
  await browser.releaseActions();
}

describe("Widget Slider Stall Reproducer", () => {
  let sliderCell;
  let widgetIframe;

  it("should launch kernel and render widget", async () => {
    await waitForNotebookSynced();
    await waitForKernelReady(300000);

    const status = await getKernelStatus();
    expect(status).toBe("idle");

    const cells = await $$('[data-cell-type="code"]');
    expect(cells.length).toBeGreaterThanOrEqual(2);

    sliderCell = cells[0];
    const executeButton = await sliderCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    // Wait for kernel to finish executing (ipywidgets init can be slow)
    await browser.waitUntil(
      async () => (await getKernelStatus()) === "idle",
      { timeout: 120000, interval: 500, timeoutMsg: "Kernel not idle after slider cell execution" },
    );

    // Widget renders inside an isolated iframe — wait for it to appear
    await browser.waitUntil(
      async () => {
        widgetIframe = await findWidgetIframe(sliderCell);
        return widgetIframe !== null;
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "Widget iframe did not appear within 30s after kernel idle",
      },
    );

    console.log("[slider-stall] Widget iframe detected in output area");
  });

  it("should survive rapid arrow-key input on focused slider", async () => {
    const cells = await $$('[data-cell-type="code"]');
    sliderCell = cells[0];

    // Re-find the iframe in case DOM changed between tests
    widgetIframe = await findWidgetIframe(sliderCell);
    if (!widgetIframe) {
      console.log("[slider-stall] Widget iframe not found, skipping arrow key test");
      return;
    }

    // Click the iframe to give it focus — the slider inside should
    // receive keyboard events. We click near the center where the
    // slider track typically is.
    await widgetIframe.click();
    await browser.pause(500);

    // Also try Tab to focus the slider thumb inside the iframe
    await browser.keys(["Tab"]);
    await browser.pause(200);

    // Phase 1: Rapid alternating left/right arrows (100 presses, no delay)
    console.log("[slider-stall] Phase 1: rapid alternating arrows (100 presses)");
    await rapidArrowKeys(100, 0);
    await browser.pause(500);

    // Phase 2: Sustained right-arrow barrage (sweep to max)
    console.log("[slider-stall] Phase 2: sustained ArrowRight (50 presses)");
    await sustainedArrowKeys("ArrowRight", 50);
    await browser.pause(500);

    // Phase 3: Sustained left-arrow barrage (sweep back)
    console.log("[slider-stall] Phase 3: sustained ArrowLeft (50 presses)");
    await sustainedArrowKeys("ArrowLeft", 50);
    await browser.pause(500);

    // Phase 4: Another rapid alternating burst
    console.log("[slider-stall] Phase 4: rapid alternating arrows (200 presses)");
    await rapidArrowKeys(200, 0);

    // Let the system settle — queued comm_msg updates drain
    console.log("[slider-stall] Settling for 3s after arrow keys...");
    await browser.pause(3000);
  });

  it("should respond to new execution after rapid input (stall detector)", async () => {
    const cells = await $$('[data-cell-type="code"]');
    expect(cells.length).toBeGreaterThanOrEqual(2);
    const verifyCell = cells[1];

    await setCellSource(verifyCell, "import random; print(f'alive-{random.random():.6f}')");

    const executeButton = await verifyCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });

    console.log("[slider-stall] Executing verification cell...");
    const execStart = Date.now();
    await executeButton.click();

    let output;
    try {
      output = await waitForCellOutput(verifyCell, 30000);
    } catch {
      const elapsed = Math.round((Date.now() - execStart) / 1000);
      const status = await getKernelStatus();
      console.error(
        `[slider-stall] STALL DETECTED: no output after ${elapsed}s (kernel: ${status})`,
      );
      throw new Error(
        `Widget sync stall detected: verification cell got no output after ${elapsed}s (kernel: ${status})`,
      );
    }

    const elapsed = Math.round((Date.now() - execStart) / 1000);
    console.log(`[slider-stall] Verification output in ${elapsed}s: ${output}`);
    expect(output).toContain("alive-");

    await browser.waitUntil(
      async () => (await getKernelStatus()) === "idle",
      { timeout: 30000, interval: 300, timeoutMsg: "Kernel not idle after verification" },
    );
  });
});
