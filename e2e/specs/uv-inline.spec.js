/**
 * E2E Test: UV Inline Dependencies
 *
 * Verifies that notebooks with inline UV dependencies get a cached
 * environment with those deps installed (not the prewarmed pool).
 *
 * Fixture: 2-uv-inline.ipynb (has requests dependency)
 *
 * Flow: Notebooks with inline deps are untrusted by default. The kernel
 * won't auto-launch until the user approves the trust dialog. This spec
 * triggers execution to surface the dialog, approves it, then verifies
 * the kernel starts with the correct inline environment.
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
  it("should start kernel after trust approval", async () => {
    console.log("[uv-inline] Waiting for notebook to sync...");
    await waitForNotebookSynced();

    // For untrusted notebooks, the kernel won't auto-launch.
    // Trigger execution to surface the trust dialog.
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Clicked execute to trigger trust dialog");

    // Approve the trust dialog (inline deps require approval)
    const approved = await approveTrustDialog(30000);
    if (approved) {
      console.log("[uv-inline] Trust dialog approved");
    } else {
      console.log(
        "[uv-inline] No trust dialog appeared (may already be trusted)",
      );
    }

    // Now wait for kernel to be ready (300s for env creation on cold CI)
    console.log("[uv-inline] Waiting for kernel ready (up to 300s)...");
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

  it("should execute code in inline environment", async () => {
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    const output = await waitForCellOutput(codeCell, 60000);
    console.log(`[uv-inline] Cell output: ${output}`);

    // Should be a cached inline env (inline-* path)
    expect(output).toContain("inline-");
  });

  it("should be able to import inline dependency", async () => {
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];

    await setCellSource(cell, "import requests; print(requests.__version__)");

    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    const output = await waitForCellOutput(cell, 30000);
    console.log(`[uv-inline] Import test output: ${output}`);

    // Should show a version number (e.g., "2.31.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
