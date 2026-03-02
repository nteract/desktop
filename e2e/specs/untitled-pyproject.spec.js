/**
 * E2E Test: Untitled Notebook with pyproject.toml
 *
 * Verifies that untitled notebooks can detect pyproject.toml
 * when launched with --cwd pointing to a project directory.
 *
 * This test opens a fresh untitled notebook with --cwd set to
 * a fixture directory containing pyproject.toml with pandas.
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

describe("Untitled Notebook with pyproject.toml", () => {
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
