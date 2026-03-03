/**
 * E2E Test: Untitled Notebook with pyproject.toml
 *
 * Verifies that untitled notebooks can detect pyproject.toml
 * when launched from a project directory.
 *
 * The app captures its working directory at startup and uses it
 * for project file detection. This test runs the app from a fixture
 * directory containing pyproject.toml with pandas.
 *
 * Run with: ./e2e/dev.sh test-untitled-pyproject
 */

import { browser } from "@wdio/globals";
import {
  setupCodeCell,
  typeSlowly,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

// FIXME: pyproject.toml deps not being installed - pandas import fails
// See: https://github.com/nteract/desktop/pull/487
describe.skip("Untitled Notebook with pyproject.toml", () => {
  it("should auto-launch kernel with project deps", async () => {
    // Wait for kernel to auto-launch using pyproject deps (120s, includes uv sync)
    await waitForKernelReady(120000);
  });

  it("should have project deps available (pandas from pyproject.toml)", async () => {
    const cell = await setupCodeCell();
    await typeSlowly("import pandas; print(pandas.__version__)");
    await browser.keys(["Shift", "Enter"]);

    // Wait for version output
    const output = await waitForCellOutput(cell, 60000);
    // Should show pandas version (e.g., "2.1.0")
    expect(output).toMatch(/^\d+\.\d+/);
  });
});
