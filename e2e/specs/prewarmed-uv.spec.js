/**
 * E2E Test: Prewarmed Environment Pool
 *
 * Verifies that basic Python notebooks use prewarmed environments
 * from the daemon's pool for fast startup.
 *
 * Fixture: 1-vanilla.ipynb (no inline deps, no project file)
 */

import {
  executeFirstCell,
  isManagedEnv,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

describe("Prewarmed Environment Pool", () => {
  it("should auto-launch kernel from pool", async () => {
    // Wait for kernel to auto-launch (90s for first startup)
    await waitForKernelReady(300000);
  });

  it("should execute code and show managed env path", async () => {
    // Execute the cell which prints sys.executable
    const cell = await executeFirstCell();

    // Wait for output
    const output = await waitForCellOutput(cell, 60000);

    // Verify it's a managed environment (runtimed-uv-* or runtimed-conda-*)
    expect(isManagedEnv(output)).toBe(true);
    // Should be from daemon's prewarmed pool
    expect(output).toMatch(/runtimed-(uv|conda)/);
  });
});
