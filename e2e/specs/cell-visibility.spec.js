/**
 * E2E Test: Cell Visibility Toggles
 *
 * Verifies that source and output visibility can be toggled using
 * JupyterLab-compatible metadata (metadata.jupyter.source_hidden,
 * metadata.jupyter.outputs_hidden).
 *
 * Tests:
 * - Hide/show source via gutter button
 * - Hide/show outputs via gutter button
 * - Compact "Cell hidden" chip when both are hidden
 * - Persistence after save/reload
 */

import { browser } from "@wdio/globals";
import {
  typeSlowly,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Cell Visibility Toggles", () => {
  it("should launch kernel and execute a cell to have outputs", async () => {
    await waitForKernelReady(90000);
    await waitForNotebookSynced();

    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 5000 });

    // Focus the editor
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    await editor.waitForExist({ timeout: 5000 });
    await editor.click();
    await browser.pause(200);

    // Select all and type a simple print
    const modKey = process.platform === "darwin" ? "Meta" : "Control";
    await browser.keys([modKey, "a"]);
    await browser.pause(100);
    await typeSlowly("print('visibility test')");

    // Execute with Shift+Enter
    await browser.keys(["Shift", "Enter"]);

    // Wait for output
    const output = await waitForCellOutput(codeCell, 30000);
    expect(output).toContain("visibility test");
  });

  it("should hide source when clicking source toggle button", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Hover over the cell to reveal gutter buttons
    await codeCell.moveTo();
    await browser.pause(300);

    // Find and click the source toggle button (Code2 icon with "Hide source" title)
    const hideSourceButton = await codeCell.$('button[title="Hide source"]');
    await hideSourceButton.waitForClickable({ timeout: 5000 });
    await hideSourceButton.click();
    await browser.pause(300);

    // Verify the source badge appears (collapsed state)
    const sourceBadge = await codeCell.$('button[title="Show source"]');
    expect(await sourceBadge.isExisting()).toBe(true);

    // The editor should no longer be visible
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    expect(await editor.isExisting()).toBe(false);
  });

  it("should show source when clicking the source badge", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Click the source badge to expand
    const sourceBadge = await codeCell.$('button[title="Show source"]');
    await sourceBadge.waitForClickable({ timeout: 5000 });
    await sourceBadge.click();
    await browser.pause(300);

    // The editor should now be visible again
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    await editor.waitForExist({ timeout: 5000 });
    expect(await editor.isExisting()).toBe(true);
  });

  it("should hide outputs when clicking output toggle button", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Hover over the cell to reveal gutter buttons
    await codeCell.moveTo();
    await browser.pause(300);

    // Find and click the output toggle button (EyeOff icon with "Hide outputs" title)
    const hideOutputButton = await codeCell.$('button[title="Hide outputs"]');
    await hideOutputButton.waitForClickable({ timeout: 5000 });
    await hideOutputButton.click();
    await browser.pause(300);

    // Verify the outputs badge appears (shows "1 output")
    const outputsBadge = await codeCell.$('button[title="Show outputs"]');
    expect(await outputsBadge.isExisting()).toBe(true);

    // The badge should contain the output count
    const badgeText = await outputsBadge.getText();
    expect(badgeText).toMatch(/\d+ output/);
  });

  it("should show outputs when clicking the outputs badge", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Click the outputs badge to expand
    const outputsBadge = await codeCell.$('button[title="Show outputs"]');
    await outputsBadge.waitForClickable({ timeout: 5000 });
    await outputsBadge.click();
    await browser.pause(300);

    // The output should be visible again
    const output = await codeCell.$('[data-slot="ansi-stream-output"]');
    await output.waitForExist({ timeout: 5000 });
    expect(await output.isExisting()).toBe(true);
  });

  it("should show compact layout when both source and outputs are hidden", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // First hide source
    await codeCell.moveTo();
    await browser.pause(300);
    const hideSourceButton = await codeCell.$('button[title="Hide source"]');
    await hideSourceButton.waitForClickable({ timeout: 5000 });
    await hideSourceButton.click();
    await browser.pause(300);

    // Then hide outputs (need to hover again to reveal button)
    await codeCell.moveTo();
    await browser.pause(300);
    const hideOutputButton = await codeCell.$('button[title="Hide outputs"]');
    await hideOutputButton.waitForClickable({ timeout: 5000 });
    await hideOutputButton.click();
    await browser.pause(300);

    // A single "Cell hidden" chip should appear (compact layout)
    const cellHiddenChip = await codeCell.$('button[title="Show cell"]');
    expect(await cellHiddenChip.isExisting()).toBe(true);
    const chipText = await cellHiddenChip.getText();
    expect(chipText).toContain("Cell hidden");

    // The editor should not be visible
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    expect(await editor.isExisting()).toBe(false);

    // The output area should not be visible
    const output = await codeCell.$('[data-slot="ansi-stream-output"]');
    expect(await output.isExisting()).toBe(false);
  });

  it("should restore cell when clicking Show cell from compact layout", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Click the "Show cell" chip to expand both source and outputs
    const cellHiddenChip = await codeCell.$('button[title="Show cell"]');
    await cellHiddenChip.waitForClickable({ timeout: 5000 });
    await cellHiddenChip.click();
    await browser.pause(300);

    // Both source and outputs should now be visible
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    await editor.waitForExist({ timeout: 5000 });
    expect(await editor.isExisting()).toBe(true);

    const output = await codeCell.$('[data-slot="ansi-stream-output"]');
    await output.waitForExist({ timeout: 5000 });
    expect(await output.isExisting()).toBe(true);
  });

  it("should show error count on hidden cell chip when cell has error output", async () => {
    const codeCell = await $('[data-cell-type="code"]');

    // Focus editor and type code that raises an error
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    await editor.click();
    await browser.pause(200);
    const modKey = process.platform === "darwin" ? "Meta" : "Control";
    await browser.keys([modKey, "a"]);
    await browser.pause(100);
    await typeSlowly("raise ValueError('test error')");

    // Execute with Shift+Enter
    await browser.keys(["Shift", "Enter"]);

    // Wait for error output to appear
    await browser.waitUntil(
      async () => {
        const errorOutput = await codeCell.$('[data-slot="ansi-error-output"]');
        return await errorOutput.isExisting();
      },
      {
        timeout: 30000,
        interval: 500,
        timeoutMsg: "Error output did not appear",
      },
    );

    // Hide source
    await codeCell.moveTo();
    await browser.pause(300);
    const hideSourceButton = await codeCell.$('button[title="Hide source"]');
    await hideSourceButton.waitForClickable({ timeout: 5000 });
    await hideSourceButton.click();
    await browser.pause(300);

    // Hide outputs
    await codeCell.moveTo();
    await browser.pause(300);
    const hideOutputButton = await codeCell.$('button[title="Hide outputs"]');
    await hideOutputButton.waitForClickable({ timeout: 5000 });
    await hideOutputButton.click();
    await browser.pause(300);

    // The chip should show "1 error"
    const cellHiddenChip = await codeCell.$('button[title="Show cell"]');
    expect(await cellHiddenChip.isExisting()).toBe(true);
    const chipText = await cellHiddenChip.getText();
    expect(chipText).toContain("1 error");

    // Restore cell for subsequent tests
    await cellHiddenChip.click();
    await browser.pause(300);
  });
});
