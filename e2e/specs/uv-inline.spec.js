/**
 * E2E Test: UV Inline Dependencies
 *
 * Verifies that notebooks with inline UV dependencies get a cached
 * environment with those deps installed (not the prewarmed pool).
 *
 * Fixture: 2-uv-inline.ipynb (has requests dependency, untrusted)
 *
 * Flow: untrusted notebooks show a banner prompting dependency review.
 * Clicking "Review Dependencies" opens the trust dialog, which must be
 * approved before the kernel starts with the inline environment.
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
  it("should launch kernel after reviewing dependencies from banner", async () => {
    await waitForNotebookSynced();

    // Untrusted notebooks show a banner — click "Review Dependencies" to open trust dialog
    const reviewButton = await $('[data-testid="review-dependencies-button"]');
    await reviewButton.waitForExist({
      timeout: 30000,
      timeoutMsg:
        "Review Dependencies button not found — untrusted banner should appear for this fixture",
    });
    await reviewButton.waitForClickable({ timeout: 5000 });
    await reviewButton.click();
    console.log(
      "[uv-inline] Clicked Review Dependencies, waiting for trust dialog...",
    );

    // Approve the trust dialog that opens
    const approved = await approveTrustDialog(30000);
    expect(approved).toBe(true);
    console.log("[uv-inline] Trust dialog approved");

    // Wait for kernel to reach idle — UV env creation on cold CI can take 5+ minutes.
    // CI matrix gives this test 12 minutes total; use 600s here.
    await waitForKernelReady(600000);
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

  it("should use inline environment path", async () => {
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Executed cell for path check");

    const output = await waitForCellOutput(codeCell, 60000);
    console.log(`[uv-inline] Cell output: ${output}`);

    // Should be a cached inline env (inline-* path)
    expect(output).toContain("inline-");
  });

  it("should be able to import inline dependency", async () => {
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];
    console.log(
      `[uv-inline] Using cell index ${cells.length > 1 ? 1 : 0} for import test`,
    );

    await setCellSource(cell, "import requests; print(requests.__version__)");

    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Clicked execute for import test");

    const output = await waitForCellOutput(cell, 30000);
    console.log(`[uv-inline] Import test output: ${output}`);

    expect(output).toMatch(/^\d+\.\d+/);
  });
});
