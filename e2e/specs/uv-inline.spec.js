/**
 * E2E Test: UV Inline Dependencies
 *
 * Verifies that notebooks with inline UV dependencies get a cached
 * environment with those deps installed (not the prewarmed pool).
 *
 * Fixture: 2-uv-inline.ipynb (has requests dependency)
 *
 * Updated to use setCellSource + explicit button clicks for compatibility
 * with tauri-plugin-webdriver (synthetic keyboard events don't work).
 */

import { browser } from "@wdio/globals";
import {
  approveTrustDialog,
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("UV Inline Dependencies", () => {
  it("should auto-launch kernel (may need trust approval)", async () => {
    console.log("[uv-inline] Waiting for kernel ready (up to 300s)...");
    // Wait for kernel or trust dialog (300s for first startup + env creation)
    await waitForKernelReady(300000);
    console.log("[uv-inline] Kernel is ready");
  });

  it("should show UV badge in toolbar", async () => {
    const depsToggle = await $('[data-testid="deps-toggle"]');
    await depsToggle.waitForExist({ timeout: 10000 });

    // env-manager syncs from RuntimeStateDoc after kernel launch — poll for it
    await browser.waitUntil(
      async () => {
        const mgr = await depsToggle.getAttribute("data-env-manager");
        return mgr === "uv";
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "Expected UV badge never appeared",
      },
    );

    expect(await depsToggle.getAttribute("data-env-manager")).toBe("uv");
    expect(await depsToggle.getAttribute("data-runtime")).toBe("python");
  });

  it("should have inline deps available after trust", async () => {
    console.log("[uv-inline] Waiting for notebook to sync...");
    await waitForNotebookSynced();

    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });
    console.log("[uv-inline] Found first code cell");

    // Set cell source via CodeMirror dispatch (bypasses keyboard events)
    await setCellSource(codeCell, "import sys; print(sys.executable)");
    console.log("[uv-inline] Set cell source to print sys.executable");

    // Click the execute button explicitly
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Clicked execute button");

    // May need to approve trust dialog for inline deps
    const approved = await approveTrustDialog(15000);
    if (approved) {
      console.log(
        "[uv-inline] Trust dialog approved, waiting for kernel restart...",
      );
      // If trust dialog appeared, wait for kernel to restart with deps
      await waitForKernelReady(300000);
      console.log("[uv-inline] Kernel restarted after trust approval");

      // Re-execute after kernel restart by clicking execute button again
      const reExecuteButton = await codeCell.$(
        '[data-testid="execute-button"]',
      );
      await reExecuteButton.waitForClickable({ timeout: 5000 });
      await reExecuteButton.click();
      console.log("[uv-inline] Re-executed cell after kernel restart");
    }

    // Wait for output
    const output = await waitForCellOutput(codeCell, 60000);
    console.log(`[uv-inline] Cell output: ${output}`);

    // Should be a cached inline env (inline-* path)
    expect(output).toContain("inline-");
  });

  it("should be able to import inline dependency", async () => {
    // Find a cell to use for the import test
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];
    console.log(
      `[uv-inline] Using cell index ${cells.length > 1 ? 1 : 0} for import test`,
    );

    // Set cell source via CodeMirror dispatch (replaces typeSlowly)
    await setCellSource(cell, "import requests; print(requests.__version__)");
    console.log("[uv-inline] Set cell source to import requests");

    // Click the execute button explicitly (replaces Shift+Enter)
    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Clicked execute button for import test");

    // Wait for version output
    const output = await waitForCellOutput(cell, 30000);
    console.log(`[uv-inline] Import test output: ${output}`);

    // Should show a version number (e.g., "2.31.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
