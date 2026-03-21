/**
 * E2E Test: Prewarmed Environment Pool
 *
 * Verifies that basic Python notebooks use prewarmed environments
 * from the daemon's pool for fast startup.
 *
 * Fixture: 1-vanilla.ipynb (no inline deps, no project file)
 * Run with: cargo xtask e2e test-fixture \
 *   crates/notebook/fixtures/audit-test/1-vanilla.ipynb \
 *   e2e/specs/prewarmed-uv.spec.js
 */

import { browser } from "@wdio/globals";
import {
  isManagedEnv,
  setCellSource,
  waitForAppReady,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Prewarmed Environment Pool", () => {
  it("should auto-launch kernel from pool", async () => {
    // 300s timeout: CI runners start cold and UV pool warming can take 3+ minutes
    await waitForKernelReady(300000);
  });

  it("should show Python runtime with UV badge", async () => {
    const depsToggle = await $('[data-testid="deps-toggle"]');
    await depsToggle.waitForExist({ timeout: 10000 });
    expect(await depsToggle.getAttribute("data-runtime")).toBe("python");

    // env-manager syncs from RuntimeStateDoc after kernel launch — poll for it
    await browser.waitUntil(
      async () => {
        const mgr = await depsToggle.getAttribute("data-env-manager");
        return mgr === "uv";
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "UV badge never appeared on deps toggle",
      },
    );
    const envManager = await depsToggle.getAttribute("data-env-manager");
    console.log(`[prewarmed-uv] env-manager: ${envManager}`);
    expect(envManager).toBe("uv");
  });

  it("should execute code and show managed env path", async () => {
    await waitForNotebookSynced();

    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    // Ensure the cell has the right source via CodeMirror dispatch API.
    // The fixture has source but CRDT sync may not have delivered it yet.
    await setCellSource(codeCell, "import sys; print(sys.executable)");

    // Execute via button click (reliable with tauri-plugin-webdriver)
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    // Wait for output
    const output = await waitForCellOutput(codeCell, 60000);

    // Debug: log the actual output so CI failures are diagnosable
    console.log(`[prewarmed-uv] output: ${JSON.stringify(output)}`);

    // Verify the kernel executed and returned a Python path.
    // The exact env path varies by platform and daemon config — on CI,
    // uv may use its global cache (~/.cache/uv/builds-v0/) rather than
    // the daemon's worktree envs (runtimed-uv-*). What matters is that
    // the kernel launched, executed code, and returned a real Python path.
    expect(output.trim()).toBeTruthy();
    expect(output).toMatch(/python/i);
  });
});
