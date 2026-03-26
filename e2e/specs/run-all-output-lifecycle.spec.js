/**
 * E2E Test: Run All Output Lifecycle
 *
 * Verifies that Run All clears all cells' stale outputs up front and
 * shows new outputs as each cell completes. Regression test for the
 * rapid ctrl-enter output loss bug (PR #1201).
 *
 * Fixture: 15-run-all-output-lifecycle.ipynb
 *   - Cell 1: sleep(2) + print (slow — keeps cell 2 queued)
 *   - Cell 2: print (fast)
 *   - Both cells have pre-existing stale outputs in the fixture
 *
 * Run with: cargo xtask e2e test-fixture \
 *   crates/notebook/fixtures/audit-test/15-run-all-output-lifecycle.ipynb \
 *   e2e/specs/run-all-output-lifecycle.spec.js
 */

import { browser } from "@wdio/globals";
import {
  waitForAppReady,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Run All Output Lifecycle", () => {
  it("should load and reach idle", async () => {
    await waitForAppReady();
    await waitForKernelReady(300000);
    await waitForNotebookSynced();
  });

  it("Run All should clear stale outputs and show new results", async () => {
    const cells = await $$('[data-cell-type="code"]');
    expect(cells.length).toBeGreaterThanOrEqual(2);

    // Verify stale outputs are visible from the fixture
    const cell1OutputBefore = await waitForCellOutput(cells[0], 10000);
    expect(cell1OutputBefore).toContain("stale-output-1");

    const cell2OutputBefore = await waitForCellOutput(cells[1], 10000);
    expect(cell2OutputBefore).toContain("stale-output-2");

    // Click "Run All"
    const runAllButton = await $('[data-testid="run-all-button"]');
    await runAllButton.waitForClickable({ timeout: 5000 });
    await runAllButton.click();

    // Wait briefly for the clear to propagate — stale outputs should
    // disappear before new execution starts.
    await browser.pause(500);

    // While cell 1 is running (2s sleep), cell 2 should not show stale output.
    // Check that at least one cell has been cleared (no "stale-output" text).
    const cell2MidRun = await cells[1]
      .$('[data-testid="cell-output"]')
      .catch(() => null);
    if (cell2MidRun && (await cell2MidRun.isExisting())) {
      const midText = await cell2MidRun.getText();
      expect(midText).not.toContain("stale-output-2");
    }

    // Wait for cell 1 to complete (2s sleep + buffer)
    const cell1Output = await waitForCellOutput(cells[0], 30000);
    expect(cell1Output).toContain("cell-1-done");

    // Wait for cell 2 to complete
    const cell2Output = await waitForCellOutput(cells[1], 30000);
    expect(cell2Output).toContain("cell-2-done");
  });
});
