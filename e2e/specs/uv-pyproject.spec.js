/**
 * E2E Test: UV pyproject.toml Detection
 *
 * Verifies that notebooks in directories with pyproject.toml use
 * `uv run` to get project dependencies.
 *
 * Fixture: pyproject-project/5-pyproject.ipynb
 *          pyproject-project/pyproject.toml (has httpx)
 *
 * Updated to use setCellSource + explicit button clicks for compatibility
 * with tauri-plugin-webdriver (synthetic keyboard events don't work).
 */

import { browser } from "@wdio/globals";
import {
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("UV pyproject.toml Detection", () => {
  it("should auto-launch kernel with project deps", async () => {
    // Wait for kernel to auto-launch (300s, includes uv sync if needed)
    console.log("[uv-pyproject] Waiting for kernel to be ready...");
    await waitForKernelReady(300000);
    console.log("[uv-pyproject] Kernel is ready.");
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
  });

  it("should execute code", async () => {
    console.log("[uv-pyproject] Waiting for notebook to sync...");
    await waitForNotebookSynced();

    // Find the first code cell
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });
    console.log("[uv-pyproject] Found first code cell.");

    // Set source via CodeMirror dispatch (no keyboard events)
    await setCellSource(codeCell, "import sys; print(sys.executable)");
    console.log("[uv-pyproject] Set cell source to print sys.executable.");

    // Click the execute button
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-pyproject] Clicked execute button.");

    // Wait for output
    const output = await waitForCellOutput(codeCell, 30000);
    console.log(`[uv-pyproject] Cell output: ${output}`);

    // Should show a Python path
    expect(output).toContain("python");
  });

  it("should have project deps available (httpx)", async () => {
    // Find cells — use second cell if available, otherwise first
    const cells = await $$('[data-cell-type="code"]');
    const cell = cells.length > 1 ? cells[1] : cells[0];
    console.log(
      `[uv-pyproject] Found ${cells.length} code cell(s), using cell index ${cells.length > 1 ? 1 : 0}.`,
    );

    // Set source via CodeMirror dispatch (replaces typeSlowly + keyboard shortcuts)
    await setCellSource(cell, "import httpx; print(httpx.__version__)");
    console.log("[uv-pyproject] Set cell source to import httpx.");

    // Click the execute button
    const executeButton = await cell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    console.log("[uv-pyproject] Clicked execute button.");

    // Wait for version output
    const output = await waitForCellOutput(cell, 60000);
    console.log(`[uv-pyproject] httpx version output: ${output}`);

    // Should show httpx version (e.g., "0.27.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
