/**
 * E2E Test: Deno Kernel
 *
 * Verifies that notebooks with Deno kernelspec are detected correctly
 * and launch with the Deno runtime (not Python).
 *
 * Fixture: 10-deno.ipynb (has kernelspec.name = "deno")
 */

import {
  executeFirstCell,
  waitForCellOutput,
  waitForKernelReady,
} from "../helpers.js";

describe("Deno Kernel", () => {
  it("should auto-launch Deno kernel", async () => {
    // Wait for kernel to auto-launch (300s, includes deno bootstrap if needed)
    await waitForKernelReady(300000);
  });

  it("should execute TypeScript and show output", async () => {
    // Execute the first cell which logs "deno:ok"
    const cell = await executeFirstCell();

    // Wait for output (60s - CI can be slow)
    const output = await waitForCellOutput(cell, 60000);

    // Verify Deno executed the TypeScript code
    // (Multiple console.log calls may render as separate stream outputs,
    // so we just check that "deno:ok" appears)
    expect(output).toContain("deno:ok");
  });
});
