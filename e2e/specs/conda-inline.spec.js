/**
 * E2E Test: Conda Inline Dependencies
 *
 * Verifies that notebooks with inline conda dependencies get a cached
 * environment with those deps installed (via rattler, not the prewarmed pool).
 *
 * Fixture: 3-conda-inline.ipynb (has markupsafe dependency via conda, untrusted)
 */

import { browser } from "@wdio/globals";
import {
  approveTrustDialog,
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

/**
 * Open the trust dialog — tries the banner first, falls back to execute.
 * The banner may not appear if trust state hasn't synced yet from the daemon.
 */
async function openTrustDialog() {
  // Try banner first (fast path — no async IPC)
  const reviewButton = await $('[data-testid="review-dependencies-button"]');
  try {
    await reviewButton.waitForExist({ timeout: 10000 });
    await reviewButton.waitForClickable({ timeout: 5000 });
    await reviewButton.click();
    console.log("[conda-inline] Opened trust dialog via banner");
    return;
  } catch {
    console.log("[conda-inline] Banner not found, falling back to execute");
  }

  // Fallback: click execute to trigger trust dialog via checkTrust IPC
  const codeCell = await $('[data-cell-type="code"]');
  await codeCell.waitForExist({ timeout: 10000 });
  await setCellSource(codeCell, "print('trigger trust')");
  const executeButton = await codeCell.$('[data-testid="execute-button"]');
  await executeButton.waitForClickable({ timeout: 10000 });
  await executeButton.click();
  console.log("[conda-inline] Opened trust dialog via execute");
}

describe("Conda Inline Dependencies", () => {
  it("should launch kernel after trust approval", async () => {
    await waitForNotebookSynced();

    await openTrustDialog();

    const approved = await approveTrustDialog(60000);
    expect(approved).toBe(true);
    console.log("[conda-inline] Trust dialog approved");

    // Conda env creation via rattler on cold CI can take 8+ minutes (CI budget: 15 min)
    await waitForKernelReady(720000);
    console.log("[conda-inline] Kernel is ready");
  });

  it("should show conda badge in toolbar", async () => {
    const depsToggle = await $('[data-testid="deps-toggle"]');
    await depsToggle.waitForExist({ timeout: 10000 });

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
  });

  it("should use conda inline environment path", async () => {
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    const output = await waitForCellOutput(codeCell, 120000);
    console.log(`[conda-inline] Cell output: ${output}`);

    expect(output).toContain("conda-inline-");
  });

  it("should be able to import inline dependency", async () => {
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];

    await setCellSource(
      cell,
      "import markupsafe; print(markupsafe.__version__)",
    );

    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    const output = await waitForCellOutput(cell, 30000);
    console.log(`[conda-inline] Import test output: ${output}`);

    expect(output).toMatch(/^\d+\.\d+/);
  });
});
