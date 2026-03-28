/**
 * E2E Test: Conda Inline Dependencies
 *
 * Verifies that notebooks with inline conda dependencies get a cached
 * environment with those deps installed (via rattler, not the prewarmed pool).
 *
 * Fixture: 3-conda-inline.ipynb (has markupsafe dependency via conda, untrusted)
 *
 * Flow: untrusted notebooks don't auto-launch the kernel. Execution
 * triggers the trust dialog, which must be approved before the kernel
 * starts with the conda inline environment.
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
  it("should launch kernel after trust approval", async () => {
    // Untrusted notebooks don't auto-launch — we must trigger execution
    // to surface the trust dialog, then approve it.
    await waitForNotebookSynced();

    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    // Click execute — this triggers the trust dialog
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Clicked execute, waiting for trust dialog...");

    // Approve the trust dialog
    const approved = await approveTrustDialog(30000);
    console.log(`[conda-inline] Trust dialog approved: ${approved}`);

    // Wait for kernel (300s for conda env creation on cold CI)
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

  it("should use conda inline environment path", async () => {
    // Kernel restarted after trust approval — need to re-execute
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Executed cell for path check");

    const output = await waitForCellOutput(codeCell, 120000);
    console.log(`[conda-inline] Cell output: ${output}`);

    // Should be a cached conda inline env (conda-inline-* path)
    expect(output).toContain("conda-inline-");
  });

  it("should be able to import inline dependency", async () => {
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];
    console.log(
      `[conda-inline] Using cell index ${cells.length > 1 ? 1 : 0} for import test`,
    );

    await setCellSource(
      cell,
      "import markupsafe; print(markupsafe.__version__)",
    );

    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Clicked execute for import test");

    const output = await waitForCellOutput(cell, 30000);
    console.log(`[conda-inline] Import test output: ${output}`);

    // Should show a version number (e.g., "1.26.4")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
