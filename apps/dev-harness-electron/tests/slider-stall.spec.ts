import { _electron as electron, expect, test } from "@playwright/test";
import path from "node:path";

// Drive an ipywidgets IntSlider and verify kernel state via a probe cell.
//
// The widget-sync stall shows up as a divergence between what the frontend
// slider displays and what the kernel's `slider.value` actually is. We drive
// many ArrowRight presses at varied rates, then print(slider.value) from a
// probe cell. If they diverge after sync should have converged, the stall
// has happened.
//
// Prereqs:
//   1. `runtimed` dev daemon running
//   2. Vite dev server running on $RUNTIMED_VITE_PORT or 5174

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");
const FIXTURE = path.resolve(__dirname, "..", "fixtures", "int-slider.ipynb");

type HarnessAPI = {
  electronAPI: {
    sendRequest: (r: unknown) => Promise<unknown>;
  };
};

declare global {
  interface Window {
    __NTERACT_DEV_HARNESS_INLINE_WIDGETS__?: boolean;
  }
}

test("slider drive vs kernel round-trip", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: { ...process.env, HARNESS_NOTEBOOK_PATH: FIXTURE },
  });

  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  const window = await app.firstWindow({ timeout: 15_000 });
  window.on("pageerror", (err) => process.stderr.write(`[renderer pageerror] ${err.message}\n`));

  await window.waitForSelector("[data-cell-id]", { timeout: 30_000 });
  const cellCount = await window.locator("[data-cell-id]").count();
  process.stdout.write(`[test] cells rendered: ${cellCount}\n`);

  const launchResult = (await window.evaluate(async () => {
    return (window as unknown as HarnessAPI).electronAPI.sendRequest({
      action: "launch_kernel",
      kernel_type: "python",
      env_source: "uv:inline",
      notebook_path: null,
    });
  })) as { result: string };
  process.stdout.write(`[test] launch_kernel → ${launchResult.result}\n`);
  if (
    launchResult.result !== "kernel_launched" &&
    launchResult.result !== "kernel_already_running"
  ) {
    test.skip(true, `kernel launch failed: ${JSON.stringify(launchResult)}`);
    return;
  }

  // Identify slider + probe cells by source content.
  const cellIds = await window.evaluate(() => {
    const out: { slider: string | null; probe: string | null } = {
      slider: null,
      probe: null,
    };
    for (const el of document.querySelectorAll("[data-cell-id]")) {
      const code = el.textContent ?? "";
      // Fixture uses FloatSlider wrapped by @interact; accept either
      // FloatSlider or IntSlider for resilience.
      if (code.includes("FloatSlider(") || code.includes("IntSlider("))
        out.slider = el.getAttribute("data-cell-id");
      if (code.includes("kernel_slider_value=")) out.probe = el.getAttribute("data-cell-id");
    }
    return out;
  });
  process.stdout.write(`[test] cell ids: ${JSON.stringify(cellIds)}\n`);
  if (!cellIds.slider || !cellIds.probe) {
    test.skip(true, "fixture missing slider or probe cell");
    return;
  }

  // Clear any stale outputs then run all cells in order.
  for (const id of [cellIds.slider, cellIds.probe]) {
    await window.evaluate(async (cellId) => {
      await (window as unknown as HarnessAPI).electronAPI.sendRequest({
        action: "clear_outputs",
        cell_id: cellId,
      });
    }, id);
  }
  await window.evaluate(async () => {
    await (window as unknown as HarnessAPI).electronAPI.sendRequest({
      action: "run_all_cells",
    });
  });

  // Locate the slider. With the inline-widgets flag (default) the widget
  // renders in parent DOM; with HARNESS_INLINE_WIDGETS=0 on main it
  // renders inside the isolated iframe.
  const inlineWidgets = process.env.HARNESS_INLINE_WIDGETS !== "0";
  const widgetSelector = '[data-widget-type="IntSlider"], [data-widget-type="FloatSlider"]';
  const slider = inlineWidgets
    ? window
        .locator(`[data-cell-id="${cellIds.slider}"]`)
        .locator(widgetSelector)
        .locator("[role='slider']")
        .first()
    : window
        .frameLocator(`[data-cell-id="${cellIds.slider}"] iframe`)
        .locator("[role='slider']")
        .first();
  await slider.waitFor({ state: "attached", timeout: 60_000 });
  process.stdout.write(`[test] slider attached (${inlineWidgets ? "parent DOM" : "iframe"})\n`);

  // Rounds of drive + probe. Each round:
  //   1. Focus the slider, press ArrowRight N times with small pauses.
  //   2. Let sync settle.
  //   3. Clear probe cell outputs; run probe cell.
  //   4. Parse `kernel_slider_value=N` from probe cell output.
  //   5. Compare to DOM's aria-valuenow.
  // Any divergence is the stall signature.
  type Round = {
    round: number;
    presses: number;
    pressMs: number;
    displayed: number | null;
    kernel: number | null;
    diverged: boolean;
  };
  const rounds: Round[] = [];
  const roundsToRun = Number(process.env.HARNESS_STALL_ROUNDS ?? "4");
  const pressesPerRound = Number(process.env.HARNESS_STALL_PRESSES ?? "500");
  // Randomize right/left bursts inside each round so the oscillation
  // pattern isn't a clean rhythm the sync engine can keep up with.
  const chaos = process.env.HARNESS_STALL_CHAOS === "1";

  for (let round = 1; round <= roundsToRun; round++) {
    await slider.focus();
    const start = Date.now();
    // Back-and-forth oscillation with asymmetric drift — the manual repro
    // that triggers the stall is rapid Right/Left toggling, not single-
    // direction spam. We add a slight net drift (more rights than lefts)
    // so a full round is expected to end at a non-zero value. If displayed
    // vs kernel diverge, it means some comm_msgs got dropped or reordered.
    const mode = process.env.HARNESS_STALL_MODE ?? "keyboard";
    if (mode === "jagged") {
      // Kyle's manual repro pattern: jagged rights, then jagged lefts,
      // back and forth until it breaks. "Jagged" = irregular micro-bursts
      // within each direction, not smooth key-repeat. Each macro burst
      // is one direction (50-150 keys), decomposed into micro-bursts of
      // 3-15 keys with tiny random pauses between them. Between macro
      // bursts, a longer pause lets sync "almost settle" before the next
      // direction slams.
      let presses = 0;
      const target = pressesPerRound;
      let goingRight = true;
      while (presses < target) {
        const macroCount = 50 + Math.floor(Math.random() * 100);
        const key = goingRight ? "ArrowRight" : "ArrowLeft";
        let macroDone = 0;
        while (macroDone < macroCount && presses < target) {
          const micro = 3 + Math.floor(Math.random() * 12);
          for (let j = 0; j < micro && macroDone < macroCount; j++) {
            // eslint-disable-next-line no-await-in-loop
            await window.keyboard.press(key);
            macroDone++;
            presses++;
          }
          // tiny inter-micro pause 0-15ms
          if (Math.random() < 0.6) {
            // eslint-disable-next-line no-await-in-loop
            await window.waitForTimeout(Math.floor(Math.random() * 15));
          }
        }
        // inter-macro pause 50-250ms — lets the CRDT / comm_msg queue try
        // to catch up before the opposite direction slams.
        // eslint-disable-next-line no-await-in-loop
        await window.waitForTimeout(50 + Math.floor(Math.random() * 200));
        goingRight = !goingRight;
      }
    } else if (mode === "mouse") {
      // Mouse drag across the slider thumb — closer to the actual manual
      // repro (dragging back and forth with a trackpad). Alternates
      // direction every ~300ms.
      const box = await slider.boundingBox();
      if (!box) throw new Error("slider has no bounding box");
      const centerY = box.y + box.height / 2;
      const leftX = box.x + box.width * 0.1;
      const rightX = box.x + box.width * 0.9;
      const durationMs = 3000 + Math.floor(Math.random() * 2000);
      const endAt = Date.now() + durationMs;
      await window.mouse.move(box.x + box.width / 2, centerY);
      await window.mouse.down();
      let goingRight = true;
      while (Date.now() < endAt) {
        const steps = 10 + Math.floor(Math.random() * 30);
        await window.mouse.move(goingRight ? rightX : leftX, centerY, { steps });
        goingRight = !goingRight;
      }
      await window.mouse.up();
    } else {
      let remaining = pressesPerRound;
      while (remaining > 0) {
        const rightBurst = chaos ? 20 + Math.floor(Math.random() * 80) : 60;
        const leftBurst = chaos ? Math.floor(Math.random() * rightBurst) : 40;
        const total = rightBurst + leftBurst;
        const slice = Math.min(total, remaining);
        const r = Math.min(rightBurst, slice);
        const l = Math.min(leftBurst, slice - r);
        for (let j = 0; j < r; j++) {
          // eslint-disable-next-line no-await-in-loop
          await window.keyboard.press("ArrowRight");
        }
        for (let j = 0; j < l; j++) {
          // eslint-disable-next-line no-await-in-loop
          await window.keyboard.press("ArrowLeft");
        }
        if (chaos && Math.random() < 0.3) {
          // Short unpredictable pause — lets sync almost catch up, then
          // races the next burst against the settling state.
          // eslint-disable-next-line no-await-in-loop
          await window.waitForTimeout(20 + Math.floor(Math.random() * 80));
        }
        remaining -= r + l;
      }
    }
    const pressMs = Date.now() - start;

    // Give sync a second to settle.
    // eslint-disable-next-line no-await-in-loop
    await window.waitForTimeout(1000);

    const displayed = Number(
      (await slider.getAttribute("aria-valuenow").catch(() => null)) ?? "NaN",
    );

    // Clear + re-execute probe cell.
    await window.evaluate(async (probeId) => {
      await (window as unknown as HarnessAPI).electronAPI.sendRequest({
        action: "clear_outputs",
        cell_id: probeId,
      });
      await (window as unknown as HarnessAPI).electronAPI.sendRequest({
        action: "execute_cell",
        cell_id: probeId,
      });
    }, cellIds.probe);

    // Wait up to 10s for the probe cell's output text to appear with the
    // marker `kernel_slider_value=`.
    let kernel: number | null = null;
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      // eslint-disable-next-line no-await-in-loop
      const text = await window
        .locator(`[data-cell-id="${cellIds.probe}"]`)
        .textContent()
        .catch(() => null);
      const match = text?.match(/kernel_slider_value=(-?\d+(?:\.\d+)?)/);
      if (match) {
        kernel = Number(match[1]);
        break;
      }
      // eslint-disable-next-line no-await-in-loop
      await window.waitForTimeout(250);
    }

    const diverged = Number.isFinite(displayed) && kernel !== null && displayed !== kernel;
    rounds.push({
      round,
      presses: pressesPerRound,
      pressMs,
      displayed: Number.isFinite(displayed) ? displayed : null,
      kernel,
      diverged,
    });
    process.stdout.write(
      `[test] round ${round}: drove ${pressesPerRound} in ${pressMs}ms → displayed=${displayed} kernel=${kernel} diverged=${diverged}\n`,
    );

    if (diverged) break;
  }

  process.stdout.write(`[test] rounds summary: ${JSON.stringify(rounds)}\n`);

  const stalled = rounds.some((r) => r.diverged);
  if (stalled) {
    process.stdout.write(
      "[test] STALL OBSERVED — displayed vs kernel diverged, widget-sync broke\n",
    );
    // Surface the divergence loudly but don't fail the test yet — this is
    // a repro scaffold. Once the stall is reliably observable we flip this
    // to expect.fail, or to an expect() assertion in a dedicated test.
  } else {
    process.stdout.write(
      "[test] no stall observed across rounds — harness is driving, frontend+kernel agree\n",
    );
  }

  // Smoke sanity: last round should have something in both values.
  const last = rounds[rounds.length - 1];
  expect(last.displayed).not.toBeNull();
  expect(last.kernel).not.toBeNull();

  await app.close();
});
