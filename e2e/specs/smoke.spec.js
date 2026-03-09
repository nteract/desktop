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
  typeSlowly,
  waitForAppReady,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

describe("E2E Smoke Test", () => {
  it("should load app and show toolbar", async () => {
    await waitForAppReady();
    const toolbar = await $('[data-testid="notebook-toolbar"]');
    expect(await toolbar.isExisting()).toBe(true);
  });

  it("should auto-launch kernel and reach idle", async () => {
    // 90s timeout for first kernel launch (includes env creation)
    await waitForKernelReady(90000);
    const status = await getKernelStatus();
    expect(status).toBe("idle");
  });

  it("should execute code and show output", async () => {
    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 5000 });

    // Focus the editor
    const editor = await codeCell.$('.cm-content[contenteditable="true"]');
    await editor.waitForExist({ timeout: 5000 });
    await editor.click();
    await browser.pause(200);

    // Select all existing content (Cmd+A on macOS, Ctrl+A elsewhere)
    const modKey = process.platform === "darwin" ? "Meta" : "Control";
    await browser.keys([modKey, "a"]);
    await browser.pause(100);

    // Type a simple print statement (replaces selected content)
    await typeSlowly("print('hello from e2e')");

    // Execute with Shift+Enter
    await browser.keys(["Shift", "Enter"]);

    // Wait for output to appear (uses global fallback for WRY compatibility)
    const outputText = await waitForCellOutput(codeCell, 30000);

    // Verify output contains expected text
    expect(outputText).toContain("hello from e2e");
  });
});
