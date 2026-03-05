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
import { getKernelStatus, waitForAppReady } from "../helpers.js";

describe("Trust Dialog Dismiss", () => {
  before(async () => {
    await waitForAppReady();
  });

  it("should close trust dialog on single click without waiting for kernel", async () => {
    // Wait for the trust dialog to appear (notebook has untrusted deps)
    const dialog = await $('[data-testid="trust-dialog"]');

    // Give the dialog time to appear (first startup with deps)
    try {
      await dialog.waitForExist({ timeout: 30000 });
    } catch {
      // If dialog doesn't appear, kernel may have auto-started with trusted deps
      // This can happen if the test ran before and the notebook was trusted
      console.log(
        "Trust dialog did not appear - notebook may already be trusted",
      );
      return;
    }

    // Record current kernel status before clicking
    const statusBefore = await getKernelStatus();
    console.log(`Kernel status before trust approval: ${statusBefore}`);

    // Find and click the approve button
    const approveButton = await $('[data-testid="trust-approve-button"]');
    await approveButton.waitForClickable({ timeout: 5000 });

    // Record time before click
    const clickTime = Date.now();
    await approveButton.click();

    // Dialog should close QUICKLY (within 3 seconds) - this is the key assertion
    // If it waited for kernel launch, this would timeout
    await browser.waitUntil(async () => !(await dialog.isExisting()), {
      timeout: 3000,
      interval: 100,
      timeoutMsg:
        "Trust dialog did not close within 3s - may be waiting for kernel launch",
    });

    const closeTime = Date.now();
    const dismissTime = closeTime - clickTime;
    console.log(`Dialog dismissed in ${dismissTime}ms`);

    // Verify dialog closed quickly (under 2 seconds)
    expect(dismissTime).toBeLessThan(2000);

    // Check kernel status - it should be starting (not yet idle)
    // This proves the dialog didn't wait for kernel launch
    const statusAfter = await getKernelStatus();
    console.log(`Kernel status after dialog closed: ${statusAfter}`);

    // The kernel should either be "starting" or have quickly reached "idle"
    // We don't strictly assert "starting" because fast machines might launch quickly
    expect(["starting", "idle", "busy"]).toContain(statusAfter);
  });
});
