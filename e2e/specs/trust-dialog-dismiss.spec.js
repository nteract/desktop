/**
 * E2E Test: Trust Dialog Single-Click Dismiss
 *
 * Verifies that clicking "Trust & Start" closes the dialog immediately,
 * without waiting for kernel launch to complete.
 *
 * Regression test for: https://github.com/nteract/desktop/issues/515
 *
 * Requires: NOTEBOOK_PATH=crates/notebook/fixtures/audit-test/2-uv-inline.ipynb
 */

import { browser } from "@wdio/globals";
import {
  getKernelStatus,
  setCellSource,
  waitForAppReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Trust Dialog Dismiss", () => {
  before(async () => {
    await waitForAppReady();
    console.log("[trust-dialog-dismiss] App ready");
  });

  it("should close trust dialog on single click without waiting for kernel", async () => {
    await waitForNotebookSynced();
    console.log("[trust-dialog-dismiss] Notebook synced");

    // Find the first code cell and set source
    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });

    await setCellSource(codeCell, "print('trust test')");
    console.log("[trust-dialog-dismiss] Set cell source");

    // Click execute — this triggers tryStartKernel → checkTrust → trust dialog
    const executeButton = await codeCell.$('[data-testid="execute-button"]');
    await executeButton.waitForClickable({ timeout: 10000 });
    await executeButton.click();
    console.log("[trust-dialog-dismiss] Clicked execute");

    // Wait for the trust dialog to appear
    const dialog = await $('[data-testid="trust-dialog"]');
    await dialog.waitForExist({
      timeout: 60000,
      timeoutMsg:
        "Trust dialog did not appear — fixture notebook should have untrusted deps",
    });
    console.log("[trust-dialog-dismiss] Trust dialog appeared");

    // Wait for approve button to be ready
    const approveButton = await $('[data-testid="trust-approve-button"]');
    await approveButton.waitForEnabled({ timeout: 30000 });
    await approveButton.waitForClickable({ timeout: 5000 });

    const clickTime = Date.now();
    await approveButton.click();
    console.log("[trust-dialog-dismiss] Clicked approve");

    // Dialog should close WITHOUT waiting for kernel launch.
    // Kernel env creation takes 60-300s+. We allow 30s for the trust IPC
    // round-trip (approve_notebook_trust + checkTrust re-verify).
    await browser.waitUntil(async () => !(await dialog.isExisting()), {
      timeout: 30000,
      interval: 200,
      timeoutMsg:
        "Trust dialog did not close within 30s - may be waiting for kernel launch (regression #515)",
    });

    const dismissTime = Date.now() - clickTime;
    console.log(`[trust-dialog-dismiss] Dialog dismissed in ${dismissTime}ms`);

    // Kernel launch is fire-and-forget — status propagates via RuntimeStateDoc
    await browser.waitUntil(
      async () => {
        const s = await getKernelStatus();
        return s === "starting" || s === "idle" || s === "busy";
      },
      {
        timeout: 60000,
        interval: 500,
        timeoutMsg:
          "Kernel status never reached starting/idle/busy after trust approval",
      },
    );
    console.log(
      `[trust-dialog-dismiss] Kernel status: ${await getKernelStatus()}`,
    );
  });
});
