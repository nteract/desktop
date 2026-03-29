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

    const codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 10000 });
    await setCellSource(codeCell, "print('trust test')");

    // The trust dialog only appears when checkTrust returns "untrusted".
    // The daemon may not have synced the notebook metadata yet, so
    // retry clicking execute until the dialog appears.
    let dialogFound = false;
    for (let attempt = 1; attempt <= 6; attempt++) {
      console.log(
        `[trust-dialog-dismiss] Execute attempt ${attempt} — triggering trust check`,
      );

      const executeButton = await codeCell.$('[data-testid="execute-button"]');
      await executeButton.waitForClickable({ timeout: 10000 });
      await executeButton.click();

      // Wait for trust dialog — short timeout per attempt, retry if not found
      try {
        const dialog = await $('[data-testid="trust-dialog"]');
        await dialog.waitForExist({ timeout: 15000 });
        dialogFound = true;
        console.log(
          `[trust-dialog-dismiss] Trust dialog appeared on attempt ${attempt}`,
        );
        break;
      } catch {
        console.log(
          `[trust-dialog-dismiss] Dialog not found on attempt ${attempt}, retrying...`,
        );
        await browser.pause(5000);
      }
    }

    if (!dialogFound) {
      throw new Error(
        "Trust dialog never appeared after 6 attempts — daemon may not have synced notebook metadata",
      );
    }

    // Now test the dismiss behavior
    const dialog = await $('[data-testid="trust-dialog"]');
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
