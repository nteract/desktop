/**
 * E2E Test: Conda Inline Dependencies
 *
 * Verifies that notebooks with inline conda dependencies get a cached
 * environment with those deps installed (via rattler, not the prewarmed pool).
 *
 * Fixture: 3-conda-inline.ipynb (has markupsafe dependency via conda)
 */

import { browser } from "@wdio/globals";
import {
  approveTrustDialog,
  executeFirstCell,
  typeSlowly,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

describe("Conda Inline Dependencies", () => {
  it("should auto-launch kernel (may need trust approval)", async () => {
    // Wait for kernel or trust dialog (120s for first startup + conda env creation)
    await waitForKernelReady(180000);
  });

  it("should have inline deps available after trust", async () => {
    // Execute the first cell (prints sys.executable)
    const cell = await executeFirstCell();

    // May need to approve trust dialog for inline deps
    const approved = await approveTrustDialog(15000);
    if (approved) {
      // If trust dialog appeared, wait for kernel to restart with deps
      await waitForKernelReady(180000);
      // Re-execute after kernel restart
      await browser.keys(["Shift", "Enter"]);
    }

    // Wait for output
    const output = await waitForCellOutput(cell, 120000);

    // Should be a cached conda inline env (conda-inline-* path)
    expect(output).toContain("conda-inline-");
  });

  it("should be able to import inline dependency", async () => {
    // Find a cell and type import test
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];

    const editor = await cell.$('.cm-content[contenteditable="true"]');
    await editor.click();
    await browser.pause(200);

    // Select all and type import
    const modKey = process.platform === "darwin" ? "Meta" : "Control";
    await browser.keys([modKey, "a"]);
    await browser.pause(100);

    await typeSlowly("import markupsafe; print(markupsafe.__version__)");
    await browser.keys(["Shift", "Enter"]);

    // Wait for version output
    const output = await waitForCellOutput(cell, 30000);
    // Should show a version number (e.g., "1.26.4")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
