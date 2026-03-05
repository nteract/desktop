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

import { browser, expect } from "@wdio/globals";
import {
  executeFirstCell,
  getKernelStatus,
  waitForAppReady,
} from "../helpers.js";

describe("Trust Dialog Dismiss", () => {
  before(async () => {
    await waitForAppReady();
  });

  it("should close trust dialog on single click without waiting for kernel", async () => {
    // Execute a cell to trigger kernel start (which requires trust approval)
    // This is how other trust-related specs trigger the dialog
    await executeFirstCell();

    // Wait for the trust dialog to appear (notebook has untrusted deps)
    const dialog = await $('[data-testid="trust-dialog"]');

    // Dialog MUST appear for this fixture - fail if it doesn't
    await dialog.waitForExist({
      timeout: 30000,
      timeoutMsg:
        "Trust dialog did not appear - fixture notebook should have untrusted deps",
    });

    // Record current kernel status before clicking
    const statusBefore = await getKernelStatus();
    console.log(`Kernel status before trust approval: ${statusBefore}`);

    // Find and click the approve button
    const approveButton = await $('[data-testid="trust-approve-button"]');
    await approveButton.waitForClickable({ timeout: 5000 });

    const clickTime = Date.now();
    await approveButton.click();

    // Dialog should close QUICKLY (within 3 seconds) - this is the key assertion
    // If it waited for kernel launch, this would timeout
    await browser.waitUntil(async () => !(await dialog.isExisting()), {
      timeout: 3000,
      interval: 100,
      timeoutMsg:
        "Trust dialog did not close within 3s - may be waiting for kernel launch (regression #515)",
    });

    const closeTime = Date.now();
    const dismissTime = closeTime - clickTime;
    console.log(`Dialog dismissed in ${dismissTime}ms`);

    // Check kernel status - should be starting or have quickly reached idle
    const statusAfter = await getKernelStatus();
    console.log(`Kernel status after dialog closed: ${statusAfter}`);
    expect(["starting", "idle", "busy"]).toContain(statusAfter);
  });
});
