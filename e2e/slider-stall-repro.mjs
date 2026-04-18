#!/usr/bin/env node
/**
 * Widget-sync stall reproducer.
 *
 * Drives the same input pattern that reproduces the slider stall in
 * manual testing — a `matplotlib @interact FloatSlider` with rapid
 * arrow-key input — but does it headlessly via WebDriver so it can be
 * iterated on without someone sitting at a keyboard.
 *
 * What it does:
 *
 * 1. Waits for the Tauri app + daemon + kernel to all be ready.
 * 2. Drops a `matplotlib @interact` cell into the first code cell.
 * 3. Executes the cell so the widget renders.
 * 4. Finds the FloatSlider thumb and hammers ArrowRight key presses
 *    at up to ~60Hz (matching OS key-repeat for arrow keys).
 * 5. Watches the rendered plot title (which shows `sin(X.YYx)`) and
 *    compares to the slider value. They should converge quickly
 *    after key presses stop. A gap that never closes is the stall.
 *
 * Paired with the `[frame-trace]` instrumentation in #1884 + #1886,
 * running this with `RUST_LOG=trace` gives a complete timeline of
 * where the stall happens when it does.
 *
 * Prereqs (see e2e/README.md for context):
 *
 *   # Build the app with the e2e-webdriver feature:
 *   cargo build --features e2e-webdriver -p notebook
 *
 *   # Start the dev daemon (in a separate terminal):
 *   cargo xtask dev-daemon
 *
 *   # Start the app with a scratch notebook (in a separate terminal).
 *   # Path doesn't matter — we rewrite the first cell.
 *   RUST_LOG=trace ./target/debug/notebook notebooks/scratch.ipynb
 *
 *   # Then run this harness:
 *   node e2e/slider-stall-repro.mjs
 *
 * Options:
 *
 *   --duration 30      How long to hammer the slider (seconds).
 *                      Default: 15.
 *   --presses-per-sec 60
 *                      Target arrow-key rate. Slower than real key
 *                      repeat to leave room for JS to process.
 *                      Default: 60.
 *   --converge-timeout 10
 *                      After the hammer phase ends, how long to
 *                      wait for the plot to catch up to the slider
 *                      value before declaring a stall.
 *                      Default: 10 seconds.
 */

import { remote } from "webdriverio";

const WEBDRIVER_PORT = Number(process.env.WEBDRIVER_PORT || 4445);
const DURATION_SECS = Number(argFlag("--duration", 15));
const PRESSES_PER_SEC = Number(argFlag("--presses-per-sec", 60));
const CONVERGE_TIMEOUT_SECS = Number(argFlag("--converge-timeout", 10));

function argFlag(name, fallback) {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const SLIDER_CELL_SOURCE = `
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
from ipywidgets import interact, FloatSlider

@interact(freq=FloatSlider(min=0.5, max=5.0, step=0.01, value=1.0, description="Frequency"))
def plot(freq):
    x = np.linspace(0, 2 * np.pi, 200)
    plt.figure(figsize=(8, 4))
    plt.plot(x, np.sin(freq * x))
    plt.title(f"sin({freq:.2f}x)")
    plt.ylim(-1.5, 1.5)
    plt.show()
`.trim();

async function main() {
  console.log(`[slider-stall] WebDriver on port ${WEBDRIVER_PORT}`);
  console.log(`[slider-stall] hammer: ${DURATION_SECS}s @ ${PRESSES_PER_SEC} Hz`);
  console.log(`[slider-stall] converge timeout: ${CONVERGE_TIMEOUT_SECS}s`);

  const browser = await remote({
    hostname: "localhost",
    port: WEBDRIVER_PORT,
    capabilities: {},
    logLevel: "warn",
  });

  try {
    // 1. App + sync
    await browser.waitUntil(
      () =>
        browser.execute(() => {
          const el = document.querySelector("[data-notebook-synced]");
          return el?.getAttribute("data-notebook-synced") === "true";
        }),
      { timeout: 30000, interval: 500, timeoutMsg: "notebook not synced" },
    );
    console.log("[slider-stall] notebook synced");

    // 2. Kernel
    await browser.waitUntil(
      async () => {
        const status = await browser.execute(() => {
          const el = document.querySelector("[data-testid='kernel-status']");
          return el?.textContent?.toLowerCase() || "";
        });
        return status === "idle" || status === "busy";
      },
      { timeout: 120000, interval: 500, timeoutMsg: "kernel not ready" },
    );
    console.log("[slider-stall] kernel ready");

    // 3. Drop the slider cell into the first code cell
    const setOk = await browser.execute((src) => {
      const cells = document.querySelectorAll('[data-cell-type="code"]');
      const cell = cells[0];
      if (!cell) return false;
      const cm = cell.querySelector(".cm-content");
      // biome-ignore lint/suspicious/noExplicitAny: CodeMirror view escape hatch
      const view = cm?.cmView?.view;
      if (!view) return false;
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: src },
      });
      return true;
    }, SLIDER_CELL_SOURCE);
    if (!setOk) {
      throw new Error("failed to set cell source");
    }
    console.log("[slider-stall] cell source set");

    // 4. Execute
    const execOk = await browser.execute(() => {
      const cells = document.querySelectorAll('[data-cell-type="code"]');
      const btn = cells[0].querySelector('[data-testid="execute-button"]');
      if (!btn) return false;
      btn.click();
      return true;
    });
    if (!execOk) throw new Error("failed to click execute");

    // 5. Wait for the slider widget to render inside the cell's output
    //    iframe. The widget is rendered inside the isolated iframe;
    //    we find it by the FloatSlider component's `role="slider"`
    //    attribute (ipywidgets renders an <input type="range"> which
    //    reports that role natively).
    await browser.waitUntil(
      async () => {
        return await browser.execute(() => {
          // Search inside all iframes for a slider input.
          // biome-ignore lint/suspicious/noExplicitAny: iframe contentWindow is platform
          const iframes = document.querySelectorAll("iframe");
          for (const f of iframes) {
            try {
              const doc = f.contentDocument;
              if (!doc) continue;
              const slider = doc.querySelector('input[type="range"], [role="slider"]');
              if (slider) return true;
            } catch {
              // cross-origin iframe, skip
            }
          }
          return false;
        });
      },
      { timeout: 60000, interval: 500, timeoutMsg: "slider not rendered" },
    );
    console.log("[slider-stall] slider rendered");

    // 6. Focus the slider and hammer ArrowRight keypresses.
    //    We use `view.actions()` style keydowns on the input element
    //    for genuine key events (not synthetic dispatchEvent, which
    //    bypasses the React handler the real UI uses).
    await browser.execute(() => {
      // biome-ignore lint/suspicious/noExplicitAny: same escape hatch as above
      const iframes = document.querySelectorAll("iframe");
      for (const f of iframes) {
        try {
          const doc = f.contentDocument;
          const slider = doc?.querySelector('input[type="range"], [role="slider"]');
          if (slider instanceof HTMLElement) {
            slider.focus();
            return true;
          }
        } catch {}
      }
      return false;
    });

    console.log("[slider-stall] hammer phase starting");
    const intervalMs = Math.max(1, Math.floor(1000 / PRESSES_PER_SEC));
    const end = Date.now() + DURATION_SECS * 1000;
    let presses = 0;
    while (Date.now() < end) {
      await browser.keys(["ArrowRight"]);
      presses++;
      // Yield so the event loop can drain React work between presses.
      if (presses % 10 === 0) await browser.pause(intervalMs);
    }
    const hammerMs = Date.now() - (end - DURATION_SECS * 1000);
    console.log(
      `[slider-stall] hammer done: ${presses} ArrowRight presses in ${hammerMs}ms` +
        ` (${((presses * 1000) / hammerMs).toFixed(1)} Hz)`,
    );

    // 7. Convergence check. Read slider value + plot title. They
    //    should match within a step or two of each other; if the
    //    plot title is frozen at a low value while the slider is
    //    high, the stall reproduced.
    const readState = async () => {
      return await browser.execute(() => {
        // biome-ignore lint/suspicious/noExplicitAny: ditto
        const iframes = document.querySelectorAll("iframe");
        for (const f of iframes) {
          try {
            const doc = f.contentDocument;
            if (!doc) continue;
            const slider = doc.querySelector('input[type="range"], [role="slider"]');
            if (!slider) continue;
            const sliderValue = Number(
              // biome-ignore lint/suspicious/noExplicitAny: ditto
              (slider).getAttribute("aria-valuenow") ??
                (slider).value ??
                "NaN",
            );
            // Plot title rendered as SVG text usually; fall back to
            // searching text content for "sin(".
            const titleEl = doc.querySelector("svg text, .plot-title");
            const titleText = titleEl?.textContent ?? doc.body?.textContent ?? "";
            const match = titleText.match(/sin\(([-\d.]+)x\)/);
            const plotValue = match ? Number(match[1]) : null;
            return { sliderValue, plotValue, titleText: match?.[0] ?? null };
          } catch {}
        }
        return null;
      });
    };

    const convergeStart = Date.now();
    const convergeDeadline = convergeStart + CONVERGE_TIMEOUT_SECS * 1000;
    let lastState = null;
    let converged = false;
    while (Date.now() < convergeDeadline) {
      const state = await readState();
      if (state) {
        lastState = state;
        if (
          state.plotValue != null &&
          Math.abs(state.sliderValue - state.plotValue) < 0.05
        ) {
          converged = true;
          break;
        }
      }
      await browser.pause(100);
    }
    const convergeMs = Date.now() - convergeStart;

    console.log("[slider-stall] ===================");
    if (converged) {
      console.log(
        `[slider-stall] converged after ${convergeMs}ms: slider=${lastState?.sliderValue}, plot=${lastState?.plotValue}`,
      );
    } else {
      console.log(
        `[slider-stall] DID NOT CONVERGE within ${CONVERGE_TIMEOUT_SECS}s: ` +
          `slider=${lastState?.sliderValue} plot=${lastState?.plotValue} title=${lastState?.titleText}`,
      );
      console.log(
        "[slider-stall] this is the stall signature. Check runtimed.log + notebook.log for [frame-trace] lines.",
      );
      process.exitCode = 1;
    }
  } finally {
    await browser.deleteSession();
  }
}

main().catch((err) => {
  console.error("[slider-stall] FAILED:", err?.message || err);
  process.exit(1);
});
