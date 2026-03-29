/**
 * E2E Test: UV Inline Dependencies
 *
 * Verifies that notebooks with inline UV dependencies get a cached
 * environment with those deps installed (not the prewarmed pool).
 *
 * Fixture: 2-uv-inline.ipynb (has requests dependency, untrusted)
 */

import { browser } from "@wdio/globals";
import {
  setCellSource,
  waitForCellOutput,
  waitForKernelReadyWithTrust,
  waitForNotebookSynced,
} from "../helpers.js";

describe("UV Inline Dependencies", () => {
  it("should launch kernel (approving trust if needed)", async () => {
    await waitForNotebookSynced();

    // The trust dialog may or may not appear depending on daemon mode.
    // waitForKernelReadyWithTrust handles both cases: it polls for
    // kernel ready and approves the trust dialog if it appears.
    const trustApproved = await waitForKernelReadyWithTrust(600000);
    console.log(
      `[uv-inline] Kernel is ready (trust approved: ${trustApproved})`,
    );
  });

  it("should show UV badge in toolbar", async () => {
    const depsToggle = await $('[data-testid="deps-toggle"]');
    await depsToggle.waitForExist({ timeout: 10000 });

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

    expect(output).toMatch(/^\d+\.\d+/);
  });
});
