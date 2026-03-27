#!/usr/bin/env node
/**
 * Stress test for automerge sync under multi-peer load.
 *
 * Drives the Tauri WebView via WebDriver to rapidly edit cells via
 * CodeMirror's dispatch API (browser.execute), simulating a frontend
 * user while an MCP agent hammers the backend concurrently.
 *
 * Usage:
 *   node e2e/stress-sync.mjs [--duration 30]
 *
 * Prerequisites:
 *   - E2E app running: ./target/debug/notebook <notebook>
 *   - WebDriver server on port 4445 (or WEBDRIVER_PORT env var)
 *   - Dev daemon running (cargo xtask dev-daemon or supervisor)
 *   - webdriverio installed: cd e2e && npm install webdriverio
 *
 * Check daemon logs after for 'automerge panicked' or '[PANIC]' lines.
 */

import { remote } from "webdriverio";

const WEBDRIVER_PORT = Number(process.env.WEBDRIVER_PORT || 4445);
const DURATION_SECS = Number(
  process.argv.includes("--duration")
    ? process.argv[process.argv.indexOf("--duration") + 1]
    : 30,
);

async function main() {
  console.log(`Connecting to WebDriver on port ${WEBDRIVER_PORT}`);
  console.log(`Duration: ${DURATION_SECS}s`);

  const browser = await remote({
    hostname: "localhost",
    port: WEBDRIVER_PORT,
    capabilities: {},
    logLevel: "warn",
  });

  console.log("Connected. Waiting for app ready...");

  // Wait for notebook to be synced
  await browser.waitUntil(
    async () =>
      browser.execute(() => {
        const el = document.querySelector("[data-notebook-synced]");
        return el?.getAttribute("data-notebook-synced") === "true";
      }),
    { timeout: 30000, interval: 500 },
  );
  console.log("Notebook synced.");

  // Wait for kernel
  await browser.waitUntil(
    async () => {
      const status = await browser.execute(() => {
        const el = document.querySelector("[data-testid='kernel-status']");
        return el?.textContent?.toLowerCase() || "";
      });
      return status === "idle" || status === "busy";
    },
    { timeout: 60000, interval: 500 },
  );
  console.log("Kernel ready.");

  const cellCount = await browser.execute(() => {
    return document.querySelectorAll('[data-cell-type="code"]').length;
  });
  console.log(`Found ${cellCount} code cells.`);

  if (cellCount === 0) {
    console.error("No code cells found.");
    await browser.deleteSession();
    process.exit(1);
  }

  // Helper: type into a cell via CodeMirror dispatch (works in wry)
  async function typeInCell(cellIndex, text) {
    return browser.execute(
      (idx, t) => {
        const cells = document.querySelectorAll('[data-cell-type="code"]');
        const cell = cells[idx];
        if (!cell) return false;
        const cm = cell.querySelector(".cm-content");
        if (!cm?.cmView?.view) return false;
        const view = cm.cmView.view;
        const pos = view.state.doc.length;
        view.dispatch({ changes: { from: pos, insert: t } });
        return true;
      },
      cellIndex,
      text,
    );
  }

  // Helper: replace cell source via CodeMirror dispatch
  async function setCellSource(cellIndex, source) {
    return browser.execute(
      (idx, src) => {
        const cells = document.querySelectorAll('[data-cell-type="code"]');
        const cell = cells[idx];
        if (!cell) return false;
        const cm = cell.querySelector(".cm-content");
        if (!cm?.cmView?.view) return false;
        const view = cm.cmView.view;
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: src },
        });
        return true;
      },
      cellIndex,
      source,
    );
  }

  // Helper: click execute button on a cell
  async function executeCell(cellIndex) {
    return browser.execute((idx) => {
      const cells = document.querySelectorAll('[data-cell-type="code"]');
      const cell = cells[idx];
      if (!cell) return false;
      const btn = cell.querySelector('[data-testid="execute-button"]');
      if (btn) {
        btn.click();
        return true;
      }
      return false;
    }, cellIndex);
  }

  const targetCell = cellCount - 1;

  // Phase 1: Rapid character-by-character typing via dispatch
  console.log("\n=== Phase 1: Rapid character typing ===");
  const code = '# stress test\nfor i in range(10):\n    print(f"line {i}")\n';
  let typed = 0;
  for (const ch of code) {
    await typeInCell(targetCell, ch);
    typed++;
    // Minimal pause — each dispatch creates an Automerge change
    await browser.pause(5);
  }
  console.log(`Typed ${typed} characters via CodeMirror dispatch.`);

  // Phase 2: Rapid source replacements (simulates formatter)
  console.log("\n=== Phase 2: Rapid source replacements ===");
  for (let i = 0; i < 20; i++) {
    await setCellSource(targetCell, `# iteration ${i}\nprint(${i})\n`);
    await browser.pause(20);
  }
  console.log("Did 20 rapid source replacements.");

  // Phase 3: Execute while typing
  console.log("\n=== Phase 3: Execute + type interleaved ===");
  for (let i = 0; i < 10; i++) {
    await setCellSource(targetCell, `print(${i})`);
    await executeCell(targetCell);
    await browser.pause(50);
    // Type during execution
    await typeInCell(targetCell, `\n# post-exec ${i}`);
    await browser.pause(50);
  }
  console.log("10 execute + type cycles.");

  // Phase 4: Sustained typing for MCP concurrent stress
  console.log(
    `\n=== Phase 4: Sustained typing (${DURATION_SECS}s for concurrent MCP) ===`,
  );
  const startTime = Date.now();
  let charCount = 0;
  while (Date.now() - startTime < DURATION_SECS * 1000) {
    // Type in bursts of 10 characters
    for (let i = 0; i < 10; i++) {
      await typeInCell(targetCell, String.fromCharCode(97 + (charCount % 26)));
      charCount++;
    }
    await browser.pause(20);

    // Occasional source replacement (simulates formatter)
    if (charCount % 100 === 0) {
      await setCellSource(
        targetCell,
        `# checkpoint ${charCount}\nprint("ok")\n`,
      );
    }
  }
  console.log(`Typed ${charCount} characters over ${DURATION_SECS}s.`);

  console.log("\n=== Done ===");
  console.log("Check daemon logs for 'automerge panicked' or '[PANIC]'.");

  await browser.deleteSession();
}

main().catch((err) => {
  console.error("Stress test failed:", err.message);
  process.exit(1);
});
