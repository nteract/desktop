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
  setCellSource,
  waitForCellOutput,
  waitForKernelReadyWithTrust,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Conda Inline Dependencies", () => {
  it("should launch kernel (approving trust if needed)", async () => {
    await waitForNotebookSynced();

    // The trust dialog may or may not appear depending on daemon mode.
    // waitForKernelReadyWithTrust handles both cases.
    // Conda env creation via rattler on cold CI can take 8+ minutes.
    const trustApproved = await waitForKernelReadyWithTrust(720000);
    console.log(
      `[conda-inline] Kernel is ready (trust approved: ${trustApproved})`,
    );
  });

  it("should show Conda badge in toolbar", async () => {
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
        timeoutMsg: "Expected Conda badge never appeared",
      },
    );

    expect(await depsToggle.getAttribute("data-env-manager")).toBe("conda");
    expect(await depsToggle.getAttribute("data-runtime")).toBe("python");
  });

  it("should use conda environment path", async () => {
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "import sys; print(sys.executable)");

    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[conda-inline] Executed cell for path check");

    const output = await waitForCellOutput(codeCell, 60000);
    console.log(`[conda-inline] Cell output: ${output}`);

    expect(output).toContain("conda");
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

    const output = await waitForCellOutput(cell, 60000);
    console.log(`[conda-inline] Import test output: ${output}`);

    expect(output).toMatch(/^\d+\.\d+/);
  });
});
