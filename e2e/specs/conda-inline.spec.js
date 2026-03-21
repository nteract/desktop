/**
 * E2E Test: Conda Inline Dependencies
 *
 * Verifies that notebooks with inline conda dependencies get a cached
 * environment with those deps installed (via rattler, not the prewarmed pool).
 *
 * Fixture: 3-conda-inline.ipynb (has markupsafe dependency via conda)
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

describe("Conda Inline Dependencies", () => {
  it("should auto-launch kernel (may need trust approval)", async () => {
    console.log("[conda-inline] Waiting for kernel ready (up to 300s)...");
    // Wait for kernel or trust dialog (300s for first startup + conda env creation)
    await waitForKernelReady(300000);
    console.log("[conda-inline] Kernel is ready");
  });

  it("should show conda badge in toolbar", async () => {
    const depsToggle = await $('[data-testid="deps-toggle"]');
    await depsToggle.waitForExist({ timeout: 10000 });

    // env-manager syncs from RuntimeStateDoc after kernel launch — poll for it
    await browser.waitUntil(
      async () => {
        const mgr = await depsToggle.getAttribute("data-env-manager");
        return mgr === "conda";
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "Expected conda badge never appeared",
      },
    );

    expect(await depsToggle.getAttribute("data-env-manager")).toBe("conda");
    expect(await depsToggle.getAttribute("data-runtime")).toBe("python");
    console.log("[conda-inline] Conda badge verified in toolbar");
  });

  it("should have inline deps available after trust", async () => {
    console.log("[conda-inline] Waiting for notebook to sync...");
    await waitForNotebookSynced();

    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });
    console.log("[conda-inline] Found first code cell");

    // Set the cell source via CodeMirror dispatch (bypasses keyboard events)
    await setCellSource(codeCell, "import sys; print(sys.executable)");
    console.log("[conda-inline] Set cell source via setCellSource");

    // Click the execute button
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Clicked execute button");

    // May need to approve trust dialog for inline deps
    const approved = await approveTrustDialog(15000);
    if (approved) {
      console.log(
        "[conda-inline] Trust dialog approved, waiting for kernel restart...",
      );
      // If trust dialog appeared, wait for kernel to restart with deps
      await waitForKernelReady(300000);
      console.log("[conda-inline] Kernel restarted after trust approval");

      // Re-execute after kernel restart by clicking the button again
      const reExecuteButton = await codeCell.$(
        '[data-testid="execute-button"]',
      );
      await reExecuteButton.waitForClickable({ timeout: 5000 });
      await reExecuteButton.click();
      console.log("[conda-inline] Re-executed cell after kernel restart");
    }

    // Wait for output
    const output = await waitForCellOutput(codeCell, 120000);
    console.log(`[conda-inline] Cell output: ${output}`);

    // Should be a cached conda inline env (conda-inline-* path)
    expect(output).toContain("conda-inline-");
  });

  it("should be able to import inline dependency", async () => {
    // Find the cells — use a second cell if available, otherwise the first
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];
    console.log(
      `[conda-inline] Using cell index ${cells.length > 1 ? 1 : 0} for import test`,
    );

    // Set the cell source directly via CodeMirror dispatch
    await setCellSource(
      cell,
      "import markupsafe; print(markupsafe.__version__)",
    );
    console.log("[conda-inline] Set import test source via setCellSource");

    // Click the execute button
    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Clicked execute button for import test");

    // Wait for version output
    const output = await waitForCellOutput(cell, 30000);
    console.log(`[conda-inline] Import test output: ${output}`);

    // Should show a version number (e.g., "1.26.4")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
