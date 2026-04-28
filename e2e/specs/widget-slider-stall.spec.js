/**
 * E2E Test: Widget Slider Stall Reproducer
 *
 * Reproduces the echo amplification bug where rapid alternating slider
 * input freezes widget-state sync. The runtime agent re-forwards stale
 * kernel echoes, creating exponential comm_msg growth that buries
 * execute_request messages in the ZMQ shell FIFO.
 *
 * Strategy:
 * 1. Execute a cell that creates a FloatSlider via @interact
 * 2. Wait for the widget to render (iframe in output area)
 * 3. Drive rapid alternating value changes via the parent-window
 *    widget update pipeline (__nteractWidgetUpdate), bypassing the
 *    security-isolated iframe. This exercises the exact same code path
 *    as real slider interaction: WidgetUpdateManager → debounced CRDT
 *    write → daemon → runtime agent → kernel.
 * 4. After rapid input, execute a second cell to verify the kernel
 *    is responsive (stall detector).
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
 * Get the comm ID of the first FloatSlider model from the widget store.
 * Returns null if no slider model found.
 */
async function getSliderCommId() {
  return await browser.execute(() => {
    const store = window.__nteractWidgetStore;
    if (!store) return null;
    const snapshot = store.getSnapshot();
    for (const [commId, model] of snapshot) {
      if (
        model.state._model_name === "FloatSliderModel" ||
        model.state._model_name === "IntSliderModel"
      ) {
        return commId;
      }
    }
    return null;
  });
}

/**
 * Drive rapid alternating slider value changes through the real widget
 * update pipeline. Uses __nteractWidgetUpdate (exposed by App.tsx for
 * E2E tests) which calls WidgetUpdateManager.updateAndPersist().
 *
 * This exercises the full chain: instant store update → debounced CRDT
 * write → sync to daemon → runtime agent diff_comm_state → comm_msg
 * to kernel → kernel @interact callback → IOPub echo → coalescing
 * writer → CRDT back to frontend.
 */
async function driveSliderValues(commId, changes) {
  await browser.execute(
    (cid, vals) => {
      const update = window.__nteractWidgetUpdate;
      if (!update) throw new Error("__nteractWidgetUpdate not available");
      for (const val of vals) {
        update(cid, { value: val });
      }
    },
    commId,
    changes,
  );
}

/**
 * Generate alternating slider values for stress testing.
 * Produces [low, high, low, high, ...] pattern that triggers echo
 * amplification when the runtime agent doesn't suppress echoes.
 */
function alternatingValues(count, low = 4.9, high = 5.1) {
  const values = [];
  for (let i = 0; i < count; i++) {
    values.push(i % 2 === 0 ? low : high);
  }
  return values;
}

describe("Widget Slider Stall Reproducer", () => {
  let sliderCell;

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
        const iframe = await findWidgetIframe(sliderCell);
        return iframe !== null;
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "Widget iframe did not appear within 30s after kernel idle",
      },
    );

    console.log("[slider-stall] Widget iframe detected in output area");

    // Wait for the widget store to have the slider model
    await browser.waitUntil(
      async () => (await getSliderCommId()) !== null,
      {
        timeout: 15000,
        interval: 500,
        timeoutMsg: "Slider model not found in widget store within 15s",
      },
    );

    const commId = await getSliderCommId();
    console.log(`[slider-stall] Slider model found: ${commId}`);
  });

  it("should survive rapid alternating value changes (echo amplification test)", async () => {
    const commId = await getSliderCommId();
    if (!commId) {
      console.log("[slider-stall] No slider model found, skipping value change test");
      return;
    }

    // Verify the E2E bridge is available
    const bridgeReady = await browser.execute(() => typeof window.__nteractWidgetUpdate === "function");
    expect(bridgeReady).toBe(true);

    // Phase 1: Rapid alternating values (100 changes, no pause)
    // This is the exact pattern that triggers echo amplification.
    console.log("[slider-stall] Phase 1: 100 rapid alternating values");
    await driveSliderValues(commId, alternatingValues(100));
    await browser.pause(500);

    // Phase 2: Another burst of 200 alternating changes
    console.log("[slider-stall] Phase 2: 200 rapid alternating values");
    await driveSliderValues(commId, alternatingValues(200));
    await browser.pause(500);

    // Phase 3: Sweep up then sweep down (unidirectional stress)
    console.log("[slider-stall] Phase 3: unidirectional sweep (50 steps each way)");
    const sweepUp = Array.from({ length: 50 }, (_, i) => 0.0 + i * 0.2);
    const sweepDown = Array.from({ length: 50 }, (_, i) => 10.0 - i * 0.2);
    await driveSliderValues(commId, [...sweepUp, ...sweepDown]);
    await browser.pause(500);

    // Phase 4: Final rapid alternating burst
    console.log("[slider-stall] Phase 4: 200 more rapid alternating values");
    await driveSliderValues(commId, alternatingValues(200));

    // Let the system settle — queued comm_msg updates drain
    console.log("[slider-stall] Settling for 3s after value changes...");
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
