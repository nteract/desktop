/**
 * E2E Test: Conda Inline Dependencies
 *
 * Verifies that notebooks with inline conda dependencies get a cached
 * environment with those deps installed (via rattler, not the prewarmed pool).
 *
 * Fixture: 3-conda-inline.ipynb (has markupsafe dependency via conda, untrusted)
 *
 * Flow: untrusted notebooks show a banner prompting dependency review.
 * Clicking "Review Dependencies" opens the trust dialog, which must be
 * approved before the kernel starts with the conda inline environment.
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
  it("should launch kernel after reviewing dependencies from banner", async () => {
    await waitForNotebookSynced();

    // Untrusted notebooks show a banner — click "Review Dependencies" to open trust dialog
    const reviewButton = await $(
      '[data-testid="review-dependencies-button"]',
    );
    await reviewButton.waitForExist({
      timeout: 30000,
      timeoutMsg:
        "Review Dependencies button not found — untrusted banner should appear for this fixture",
    });
    await reviewButton.waitForClickable({ timeout: 5000 });
    await reviewButton.click();
    console.log(
      "[conda-inline] Clicked Review Dependencies, waiting for trust dialog...",
    );

    // Approve the trust dialog that opens
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
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Executed cell for path check");

    const output = await waitForCellOutput(codeCell, 120000);
    console.log(`[conda-inline] Cell output: ${output}`);

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

    expect(output).toMatch(/^\d+\.\d+/);
  });
});
