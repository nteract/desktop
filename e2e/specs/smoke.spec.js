/**
 * E2E Smoke Test
 *
 * Minimal test that verifies the full stack works:
 * 1. App loads and shows toolbar
 * 2. Kernel auto-launches and reaches idle state
 * 3. Code can be executed and output appears
 *
 * This test requires the daemon to be running (e2e/dev.sh handles this).
 */

import { browser } from "@wdio/globals";
import {
  getKernelStatus,
  setCellSource,
  waitForAppReady,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("E2E Smoke Test", () => {
  it("should load app and show toolbar", async () => {
    await waitForAppReady();
    const toolbar = await $('[data-testid="notebook-toolbar"]');
    expect(await toolbar.isExisting()).toBe(true);
  });

  it("should auto-launch kernel and reach idle", async () => {
    // 300s timeout: CI runners start cold and UV pool warming can take 3+ minutes
    await waitForKernelReady(300000);
    const status = await getKernelStatus();
    expect(status).toBe("idle");
  });

  it("should execute code and show output", async () => {
    // Wait for the notebook to finish Automerge sync and render cells.
    await waitForNotebookSynced();

    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 5000 });

    // Set cell source via CodeMirror dispatch API (reliable with any WebDriver)
    await setCellSource(codeCell, "print('hello from e2e')");

    // Execute via the execute button (more reliable than Shift+Enter with synthetic events)
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    if (await executeButton.isExisting()) {
      await executeButton.waitForClickable({ timeout: 5000 });
      await executeButton.click();
    } else {
      // Fallback: focus editor and use Shift+Enter
      const editor = await codeCell.$('.cm-content[contenteditable="true"]');
      await editor.click();
      await browser.pause(200);
      await browser.keys(["Shift", "Enter"]);
    }

    // Wait for output to appear (uses global fallback for WRY compatibility)
    const outputText = await waitForCellOutput(codeCell, 30000);

    // Verify output contains expected text
    expect(outputText).toContain("hello from e2e");
  });
});
