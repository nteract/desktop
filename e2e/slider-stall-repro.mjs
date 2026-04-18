#!/usr/bin/env node
/**
 * Widget-sync stall reproducer.
 *
 * Drives the slider-arrow-key input pattern that reproduces the
 * widget-sync stall in manual testing, headlessly via WebDriver so
 * it can be iterated on without someone at a keyboard.
 *
 * Scope:
 *
 *   This harness drives the *input* that causes the stall and
 *   exits when the hammer phase completes. It does NOT try to
 *   detect convergence itself. The diagnosis lives in the
 *   `[frame-trace]` logs from the sibling instrumentation PRs
 *   (#1884 app/webview/relay, #1886 daemon). After a run, grep
 *   `notebook.log` + `runtimed.log` for `[frame-trace]` and
 *   compare outbound vs inbound counts.
 *
 *   Why not detect convergence in-harness? The output widget is
 *   rendered inside a sandboxed iframe (`allow-same-origin` is
 *   specifically disallowed for security — see
 *   `src/components/isolated/isolated-frame.tsx` SANDBOX_ATTRS).
 *   Parent-side `contentDocument` access is blocked. WebDriver's
 *   `switchToFrame` lets us click inside the iframe, but reliably
 *   diffing "what the client thinks" vs "what the kernel produced"
 *   from that vantage is fragile. Let the logs do the work.
 *
 * What it does:
 *
 *   1. Waits for the Tauri app + daemon + kernel to all be ready.
 *   2. Rewrites the first cell to a minimal ipywidgets slider
 *      scenario — no matplotlib, no numpy, no Agg backend. Just
 *      a FloatSlider whose change handler prints a line, so we
 *      know from the cell output whether the kernel is getting
 *      updates at all.
 *   3. Executes the cell so the widget renders.
 *   4. switchToFrame into the output iframe, focuses the slider,
 *      hammers ArrowRight key presses at ~60 Hz for the
 *      configured duration, switches back.
 *   5. Exits. Cell output + log traces tell you what happened.
 *
 * Prereqs:
 *
 *   # Build the app with the e2e-webdriver feature:
 *   cargo build --features e2e-webdriver -p notebook
 *
 *   # Start the dev daemon (in a separate terminal):
 *   cargo xtask dev-daemon
 *
 *   # The target notebook's env must have ipywidgets. Scratch
 *   # notebooks default to the prewarmed UV pool which does NOT
 *   # include ipywidgets — either add it to the notebook's
 *   # dependencies via the UI before running the harness, or use
 *   # a notebook whose env already has it.
 *
 *   # Start the app with the target notebook (in a separate terminal).
 *   RUST_LOG=trace ./target/debug/notebook <notebook.ipynb>
 *
 *   # Then run this harness:
 *   node e2e/slider-stall-repro.mjs
 *
 * Options:
 *
 *   --duration 15        How long to hammer the slider (seconds).
 *   --presses-per-sec 60 Target arrow-key rate.
 */

import { remote } from "webdriverio";

const WEBDRIVER_PORT = Number(process.env.WEBDRIVER_PORT || 4445);
const DURATION_SECS = Number(argFlag("--duration", 15));
const PRESSES_PER_SEC = Number(argFlag("--presses-per-sec", 60));

function argFlag(name, fallback) {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

// Minimal widget that exercises the RuntimeStateSync path without
// needing matplotlib, numpy, an interactive backend, or anything
// else beyond ipywidgets. The on_change handler emits a print line
// so the cell output itself tells us whether the kernel is getting
// updates.
const SLIDER_CELL_SOURCE = `
from ipywidgets import FloatSlider, Output
from IPython.display import display

_slider = FloatSlider(min=0.0, max=1000.0, step=1.0, value=0.0, description="drive")
_out = Output()

@_out.capture(clear_output=True, wait=True)
def _on_change(change):
    print(f"kernel_saw={int(change['new'])}")

_slider.observe(_on_change, names='value')
display(_slider, _out)
`.trim();

async function main() {
  console.log(`[slider-stall] WebDriver on port ${WEBDRIVER_PORT}`);
  console.log(`[slider-stall] hammer: ${DURATION_SECS}s @ ${PRESSES_PER_SEC} Hz`);

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

    // 3. Find the visually-topmost code cell and hold onto its
    //    `data-cell-id`. NotebookView keeps DOM order stable by
    //    cell id (for iframe-reload avoidance) and uses CSS
    //    `order` for the visual position the user sees, so
    //    NodeList index is NOT the visual order. Additionally, if
    //    the notebook starts with a markdown cell, the code cell
    //    is not the first `<div>` sibling — `:first-of-type`
    //    selectors won't match. Scope everything by cell id
    //    instead so the rewrite, execute, and iframe lookup all
    //    target the same cell.
    const targetCellId = await browser.execute(() => {
      const codeCells = Array.from(
        document.querySelectorAll('[data-cell-type="code"]'),
      );
      if (codeCells.length === 0) return null;
      // Pick the lowest CSS `order` value. Ties break by DOM order.
      const withOrder = codeCells.map((el, i) => ({
        id: el.getAttribute("data-cell-id"),
        order: Number.parseInt(getComputedStyle(el).order, 10) || 0,
        i,
      }));
      withOrder.sort((a, b) => a.order - b.order || a.i - b.i);
      return withOrder[0]?.id ?? null;
    });
    if (!targetCellId) {
      throw new Error("no code cells found");
    }
    console.log(`[slider-stall] target cell: ${targetCellId}`);

    const cellSelector = `[data-cell-id="${targetCellId}"]`;

    // Drop the widget cell source into the target cell.
    const setOk = await browser.execute(
      (sel, src) => {
        const cell = document.querySelector(sel);
        if (!cell) return false;
        const cm = cell.querySelector(".cm-content");
        // biome-ignore lint/suspicious/noExplicitAny: CodeMirror view escape hatch
        const view = cm?.cmView?.view;
        if (!view) return false;
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: src },
        });
        return true;
      },
      cellSelector,
      SLIDER_CELL_SOURCE,
    );
    if (!setOk) throw new Error("failed to set cell source");
    console.log("[slider-stall] cell source set");

    // 4. Execute
    const execOk = await browser.execute((sel) => {
      const cell = document.querySelector(sel);
      const btn = cell?.querySelector('[data-testid="execute-button"]');
      if (!btn) return false;
      btn.click();
      return true;
    }, cellSelector);
    if (!execOk) throw new Error("failed to click execute");

    // 5. The widget lives inside an isolated iframe. We can't see its
    //    DOM from the parent (`allow-same-origin` is intentionally
    //    off), but WebDriver's `switchToFrame` is a driver-level
    //    operation that works regardless of the sandbox.
    //
    //    Look specifically inside the FIRST cell (the one we just
    //    rewrote + executed). Scanning the global iframe list can
    //    pick up stale outputs from other cells if the notebook
    //    already had widget renders before we overwrote the cell.
    const deadline = Date.now() + 60000;
    let sliderFrame = null;
    while (Date.now() < deadline) {
      // Scope strictly to the cell we just rewrote — not by DOM
      // position (`:first-of-type` fails when a non-code cell
      // precedes this one) or global iframe scan (could pick up a
      // stale render from another cell).
      const cellIframes = await browser.$$(`${cellSelector} iframe`);
      for (const frame of cellIframes) {
        try {
          await browser.switchToFrame(frame);
          const sliderExists = await browser.execute(
            () =>
              !!document.querySelector('input[type="range"], [role="slider"]'),
          );
          if (sliderExists) {
            sliderFrame = frame;
            break;
          }
        } catch {
          // driver may refuse switch on some frames; skip
        } finally {
          if (!sliderFrame) await browser.switchToParentFrame();
        }
      }
      if (sliderFrame) break;
      await browser.pause(300);
    }

    if (!sliderFrame) {
      // Widget didn't render in the target cell. Read the cell text
      // to surface whatever actually happened (usually
      // ModuleNotFoundError for ipywidgets).
      const outputText = await browser.execute((sel) => {
        const cell = document.querySelector(sel);
        return cell?.textContent?.slice(0, 500) ?? "";
      }, cellSelector);
      throw new Error(
        `slider did not render within 60s in target cell ${targetCellId}. ` +
          `Cell text: ${outputText}`,
      );
    }
    console.log(
      `[slider-stall] slider rendered (inside cell ${targetCellId}'s iframe)`,
    );

    // 6. Focus the slider, hammer keys. Already in the iframe
    //    context from the discovery loop above.
    const sliderEl = await browser.$('input[type="range"], [role="slider"]');
    await sliderEl.click();

    console.log("[slider-stall] hammer phase starting");
    const end = Date.now() + DURATION_SECS * 1000;
    const intervalMs = 1000 / PRESSES_PER_SEC;
    // Target-time pacing so the actual rate matches --presses-per-sec
    // regardless of WebDriver latency (up to a ceiling of "as fast
    // as WebDriver can go"). Without this, sleeping every N
    // iterations undershoots the target rate by a factor of N.
    let nextTick = Date.now();
    let presses = 0;
    while (Date.now() < end) {
      await browser.keys(["ArrowRight"]);
      presses++;
      nextTick += intervalMs;
      const wait = nextTick - Date.now();
      if (wait > 0) await browser.pause(wait);
    }
    const hammerMs = DURATION_SECS * 1000;
    console.log(
      `[slider-stall] hammer done: ${presses} ArrowRight presses in ${hammerMs}ms` +
        ` (${((presses * 1000) / hammerMs).toFixed(1)} Hz)`,
    );

    // Report the slider's post-hammer aria-valuenow for context.
    const finalSliderValue = await browser.execute(() => {
      const s = document.querySelector('input[type="range"], [role="slider"]');
      // biome-ignore lint/suspicious/noExplicitAny: permissive DOM probe
      return s?.getAttribute("aria-valuenow") ?? s?.value ?? null;
    });

    await browser.switchToParentFrame();

    console.log(
      `[slider-stall] slider aria-valuenow after hammer: ${finalSliderValue}`,
    );
    console.log(
      `[slider-stall] done. grep notebook.log + runtimed.log for '[frame-trace]' to see the sync traffic.`,
    );
  } finally {
    await browser.deleteSession();
  }
}

main().catch((err) => {
  console.error("[slider-stall] FAILED:", err?.message || err);
  process.exit(1);
});
