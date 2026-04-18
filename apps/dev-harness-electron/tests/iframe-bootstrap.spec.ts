import { _electron as electron, test } from "@playwright/test";
import path from "node:path";

// Debug test: disable the inline-widgets bypass and trace the iframe
// bootstrap handshake to pinpoint where it breaks under Electron.
//
// Expected parent ↔ iframe timeline (per .claude/rules/iframe-isolation.md):
//   1. IsolatedFrame mounts, iframe loads bootstrap HTML from blob: URL
//   2. iframe sends "ready"
//   3. parent sends eval (CSS + React renderer bundle)
//   4. iframe sends "renderer_ready"
//   5. CommBridgeManager sends nteract/bridgeReady
//   6. iframe sends nteract/widgetReady
//   7. parent sends nteract/widgetSnapshot
//   8. widget renders
//
// This test watches postMessage traffic on both sides and logs timestamps
// so we can see exactly which hop stalls.

const MAIN_ENTRY = path.join(__dirname, "..", "src", "main", "index.js");
const FIXTURE = path.resolve(__dirname, "..", "fixtures", "int-slider.ipynb");

test("iframe bootstrap handshake (inline bypass disabled)", async () => {
  const app = await electron.launch({
    args: [MAIN_ENTRY],
    env: {
      ...process.env,
      HARNESS_NOTEBOOK_PATH: FIXTURE,
      HARNESS_INLINE_WIDGETS: "0", // Force the iframe path.
    },
  });

  app.process().stdout?.on("data", (d) => process.stdout.write(`[main] ${d}`));
  app.process().stderr?.on("data", (d) => process.stderr.write(`[main!] ${d}`));

  // Install the message tap BEFORE the page loads so we catch the very
  // first bootstrap frames. `addInitScript` is the Playwright equivalent
  // of page.evaluateOnNewDocument — it runs before any page script.
  // Attaching later misses the early "ready" handshake traffic exactly
  // when the bootstrap localization is most useful.
  await app.context().addInitScript(() => {
    type Entry = {
      t: number;
      direction: string;
      type: unknown;
      method?: unknown;
    };
    const log: Entry[] = [];
    (window as unknown as { __harnessMessageLog: Entry[] }).__harnessMessageLog = log;
    window.addEventListener(
      "message",
      (ev) => {
        try {
          const d = ev.data as Record<string, unknown> | null;
          log.push({
            t: performance.now(),
            direction: "parent-received",
            type: d && typeof d === "object" && "type" in d ? d.type : typeof ev.data,
            method: d && typeof d === "object" && "method" in d ? d.method : undefined,
          });
        } catch {}
      },
      true, // use capture so we see events before any other handler
    );
  });

  const window = await app.firstWindow({ timeout: 15_000 });

  await window.waitForSelector("[data-cell-id]", { timeout: 30_000 });

  await window.evaluate(async () => {
    await (
      window as unknown as { electronAPI: { sendRequest: (r: unknown) => Promise<unknown> } }
    ).electronAPI.sendRequest({
      action: "launch_kernel",
      kernel_type: "python",
      env_source: "uv:inline",
      notebook_path: null,
    });
  });
  const sliderId = await window.evaluate(() => {
    for (const el of document.querySelectorAll("[data-cell-id]")) {
      if ((el.textContent ?? "").includes("IntSlider(")) return el.getAttribute("data-cell-id");
    }
    return null;
  });
  if (!sliderId) {
    test.skip(true, "no slider cell");
    return;
  }

  await window.evaluate(async (id) => {
    const api = (
      window as unknown as { electronAPI: { sendRequest: (r: unknown) => Promise<unknown> } }
    ).electronAPI;
    await api.sendRequest({ action: "clear_outputs", cell_id: id });
    await api.sendRequest({ action: "run_all_cells" });
  }, sliderId);

  // Wait up to 30s for iframe DOM to show up in cell
  const iframeSelector = `[data-cell-id="${sliderId}"] iframe`;
  try {
    await window.waitForSelector(iframeSelector, { timeout: 30_000, state: "attached" });
  } catch {
    process.stdout.write("[test] iframe never attached\n");
  }

  // Give the bootstrap handshake another 15 seconds.
  await window.waitForTimeout(15_000);

  const messageLog = await window.evaluate(
    () => (window as unknown as { __harnessMessageLog?: unknown[] }).__harnessMessageLog ?? [],
  );
  process.stdout.write(`[test] parent message log (${messageLog.length} entries):\n`);
  for (const entry of messageLog) {
    process.stdout.write(`  ${JSON.stringify(entry)}\n`);
  }

  const iframeDims = await window.evaluate((sel) => {
    const f = document.querySelector(sel) as HTMLIFrameElement | null;
    if (!f) return null;
    return {
      w: f.clientWidth,
      h: f.clientHeight,
      src: f.src?.slice(0, 80),
    };
  }, iframeSelector);
  process.stdout.write(`[test] iframe dims: ${JSON.stringify(iframeDims)}\n`);

  const iframeBodyLen = await window
    .frameLocator(iframeSelector)
    .locator("body")
    .innerHTML()
    .then((s) => s.length)
    .catch(() => -1);
  process.stdout.write(`[test] iframe body length: ${iframeBodyLen}\n`);

  const rendererActive = await window
    .frameLocator(iframeSelector)
    .locator("body")
    .evaluate(
      () =>
        (window as unknown as { __REACT_RENDERER_ACTIVE__?: boolean }).__REACT_RENDERER_ACTIVE__,
    )
    .catch(() => "unavailable");
  process.stdout.write(`[test] iframe __REACT_RENDERER_ACTIVE__: ${rendererActive}\n`);

  await app.close();
});
