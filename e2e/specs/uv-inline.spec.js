/**
 * E2E Test: UV Inline Dependencies
 *
 * Verifies that notebooks with inline UV dependencies get a cached
 * environment with those deps installed (not the prewarmed pool).
 *
 * Fixture: 2-uv-inline.ipynb (has requests dependency, untrusted)
 *
 * Flow: untrusted notebooks don't auto-launch the kernel. Execution
 * triggers the trust dialog, which must be approved before the kernel
 * starts with the inline environment.
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
  it("should launch kernel after trust approval", async () => {
    // Untrusted notebooks don't auto-launch — we must trigger execution
    // to surface the trust dialog, then approve it.
    await waitForNotebookSynced();

    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    // Set a simple probe as the cell source
    await setCellSource(codeCell, "import sys; print(sys.executable)");

    // Click execute — this triggers the trust dialog (kernel won't start untrusted)
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-inline] Clicked execute, waiting for trust dialog...");

    // Approve the trust dialog (must appear for untrusted fixture)
    const approved = await approveTrustDialog(30000);
    console.log(`[uv-inline] Trust dialog approved: ${approved}`);

    // Now wait for kernel to reach idle (300s for UV env creation on cold CI)
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

  it("should use inline environment path", async () => {
    // The cell was set to `import sys; print(sys.executable)` in test 1
    // and executed there. But the kernel restarted after trust approval,
    // so we need to re-execute.
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

    // Should show a version number (e.g., "2.31.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
