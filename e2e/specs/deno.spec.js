/**
 * E2E Test: Deno Kernel
 *
 * Verifies that notebooks with Deno kernelspec are detected correctly
 * and launch with the Deno runtime (not Python).
 *
 * Fixture: 10-deno.ipynb (has kernelspec.name = "deno")
 * Run with: cargo xtask e2e test-fixture \
 *   crates/notebook/fixtures/audit-test/10-deno.ipynb \
 *   e2e/specs/deno.spec.js
 */

import { browser } from "@wdio/globals";
import {
  setCellSource,
  waitForCellOutput,
  waitForKernelReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Deno Kernel", () => {
  it("should auto-launch Deno kernel", async () => {
    // 300s timeout: Deno bootstrap can take a while on cold CI
    await waitForKernelReady(300000);
  });

  it("should execute TypeScript and show output", async () => {
    await waitForNotebookSynced();

    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    // Set source via CodeMirror dispatch (reliable with any WebDriver impl)
    await setCellSource(codeCell, 'console.log("deno:ok");');

    // Execute via button click
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();

    // Wait for output (60s - CI can be slow)
    const output = await waitForCellOutput(codeCell, 60000);

    // Debug: log output for CI diagnosis
    console.log(`[deno] output: ${JSON.stringify(output)}`);

    // Verify Deno executed the TypeScript code
    expect(output).toContain("deno:ok");
  });
});
