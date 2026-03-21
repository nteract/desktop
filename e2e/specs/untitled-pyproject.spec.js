/**
 * E2E Test: Untitled Notebook with pyproject.toml
 *
 * Verifies that untitled notebooks can detect pyproject.toml
 * when launched from a project directory.
 *
 * The app captures its working directory at startup and uses it
 * for project file detection. This test runs the app from a fixture
 * directory containing pyproject.toml with httpx.
 *
 * Run with: ./e2e/dev.sh test-untitled-pyproject
 *
 * Updated to use setCellSource + explicit button clicks instead of
 * setupCodeCell/typeSlowly for tauri-plugin-webdriver compatibility.
 */

import { browser } from "@wdio/globals";
import {
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Untitled Notebook with pyproject.toml", () => {
  it("should auto-launch kernel with project deps", async () => {
    console.log("[untitled-pyproject] Waiting for kernel to auto-launch...");
    // Wait for kernel to auto-launch using pyproject deps (300s, includes uv sync)
    await waitForKernelReady(300000);
    console.log("[untitled-pyproject] Kernel is ready");
  });

  it("should have project deps available (httpx from pyproject.toml)", async () => {
    console.log("[untitled-pyproject] Waiting for notebook to sync...");
    await waitForNotebookSynced();

    // Find existing code cell or create one
    let codeCell = await $('[data-cell-type="code"]');
    const cellExists = await codeCell.isExisting();
    console.log(`[untitled-pyproject] Code cell exists: ${cellExists}`);

    if (!cellExists) {
      console.log("[untitled-pyproject] No code cell found, creating one...");
      const addCodeButton = await $('[data-testid="add-code-cell-button"]');
      await addCodeButton.waitForClickable({ timeout: 5000 });
      await addCodeButton.click();
      await browser.pause(500);

      codeCell = await $('[data-cell-type="code"]');
      await codeCell.waitForExist({ timeout: 5000 });
      console.log("[untitled-pyproject] Code cell created");
    }

    // Set cell source via CodeMirror dispatch API (bypasses synthetic keyboard events)
    const code = "import httpx; print(httpx.__version__)";
    console.log(`[untitled-pyproject] Setting cell source: ${code}`);
    await setCellSource(codeCell, code);

    // Click the execute button explicitly
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    console.log("[untitled-pyproject] Clicking execute button...");
    await executeButton.click();

    // Wait for version output
    console.log("[untitled-pyproject] Waiting for cell output...");
    const output = await waitForCellOutput(codeCell, 60000);
    console.log(`[untitled-pyproject] Got output: ${output}`);

    // Should show httpx version (e.g., "0.27.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
