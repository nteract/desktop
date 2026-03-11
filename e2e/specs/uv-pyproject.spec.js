/**
 * E2E Test: UV pyproject.toml Detection
 *
 * Verifies that notebooks in directories with pyproject.toml use
 * `uv run` to get project dependencies.
 *
 * Fixture: pyproject-project/5-pyproject.ipynb
 *          pyproject-project/pyproject.toml (has httpx)
 */

import { browser } from "@wdio/globals";
import {
  executeFirstCell,
  typeSlowly,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

describe("UV pyproject.toml Detection", () => {
  it("should auto-launch kernel with project deps", async () => {
    // Wait for kernel to auto-launch (120s, includes uv sync if needed)
    await waitForKernelReady(120000);
  });

  it("should execute code", async () => {
    // Execute the first cell which prints sys.executable
    const cell = await executeFirstCell();

    // Wait for output
    const output = await waitForCellOutput(cell, 30000);

    // Should show a Python path
    expect(output).toContain("python");
  });

  it("should have project deps available (httpx)", async () => {
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

    await typeSlowly("import httpx; print(httpx.__version__)");
    await browser.keys(["Shift", "Enter"]);

    // Wait for version output
    const output = await waitForCellOutput(cell, 60000);
    // Should show httpx version (e.g., "0.27.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
