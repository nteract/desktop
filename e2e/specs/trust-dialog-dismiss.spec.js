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
  getKernelStatus,
  waitForAppReady,
  waitForNotebookSynced,
} from "../helpers.js";

describe("Trust Dialog Dismiss", () => {
  before(async () => {
    await waitForAppReady();
    console.log("[trust-dialog-dismiss] App ready");
  });

  it("should close trust dialog on single click without waiting for kernel", async () => {
    // Wait for the notebook to sync and render cells
    await waitForNotebookSynced();
    console.log("[trust-dialog-dismiss] Notebook synced");

    // Untrusted notebooks show a banner — click "Review Dependencies" to open
    // the trust dialog directly (avoids async checkTrust IPC from execute path)
    const reviewButton = await $('[data-testid="review-dependencies-button"]');
    await reviewButton.waitForExist({
      timeout: 30000,
      timeoutMsg:
        "Review Dependencies button not found — untrusted banner should appear for this fixture",
    });
    await reviewButton.waitForClickable({ timeout: 5000 });
    await reviewButton.click();
    console.log("[trust-dialog-dismiss] Clicked Review Dependencies");

    // Wait for the trust dialog to appear
    const dialog = await $('[data-testid="trust-dialog"]');
    await dialog.waitForExist({
      timeout: 15000,
      timeoutMsg:
        "Trust dialog did not appear after clicking Review Dependencies",
    });
    expect(await dialog.isExisting()).toBe(true);
    console.log("[trust-dialog-dismiss] Trust dialog appeared");

    // Record current kernel status before clicking
    const statusBefore = await getKernelStatus();
    console.log(
      `[trust-dialog-dismiss] Kernel status before trust approval: ${statusBefore}`,
    );

    // Approve and decline buttons should be present
    const approveButton = await $('[data-testid="trust-approve-button"]');
    expect(await approveButton.isExisting()).toBe(true);
    const declineButton = await $('[data-testid="trust-decline-button"]');
    expect(await declineButton.isExisting()).toBe(true);

    // Wait for the button to be enabled — a checkTrust() call from the
    // daemon:ready listener can briefly set loading=true, which disables
    // the buttons. Poll until the disabled attribute clears.
    await approveButton.waitForEnabled({ timeout: 30000 });
    await approveButton.waitForClickable({ timeout: 5000 });

    const clickTime = Date.now();
    await approveButton.click();
    console.log("[trust-dialog-dismiss] Clicked approve button");

    // Dialog should close QUICKLY (within 10 seconds) - this is the key assertion.
    // If it waited for kernel launch, this would take 30-120s+ (env creation).
    // The 10s budget accounts for CI variability: Tauri IPC round-trip to daemon
    // for approve_notebook_trust + checkTrust() re-verify + React re-render.
    await browser.waitUntil(async () => !(await dialog.isExisting()), {
      timeout: 10000,
      interval: 100,
      timeoutMsg:
        "Trust dialog did not close within 10s - may be waiting for kernel launch (regression #515)",
    });

    const closeTime = Date.now();
    const dismissTime = closeTime - clickTime;
    console.log(`[trust-dialog-dismiss] Dialog dismissed in ${dismissTime}ms`);

    // Kernel launch is fire-and-forget — status propagates via RuntimeStateDoc
    // sync, so poll briefly instead of reading once.
    await browser.waitUntil(
      async () => {
        const s = await getKernelStatus();
        return s === "starting" || s === "idle" || s === "busy";
      },
      {
        timeout: 30000,
        interval: 300,
        timeoutMsg:
          "Kernel status never reached starting/idle/busy after trust approval",
      },
    );
    const statusAfter = await getKernelStatus();
    console.log(
      `[trust-dialog-dismiss] Kernel status after dialog closed: ${statusAfter}`,
    );
  });
});
