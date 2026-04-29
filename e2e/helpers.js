/**
 * Shared E2E test helpers
 *
 * Provides smart wait functions that detect actual app/kernel readiness
 * instead of relying on arbitrary pauses.
 */

import os from "node:os";
import { browser } from "@wdio/globals";

// macOS uses Cmd (Meta) for shortcuts, Linux uses Ctrl
const MOD_KEY = os.platform() === "darwin" ? "Meta" : "Control";

/**
 * Wait for the app to be fully loaded (toolbar visible).
 * Uses browser.execute() (executeScript) which goes through the JS bridge
 * directly, avoiding potential findElement timing issues.
 */
export async function waitForAppReady() {
  await browser.waitUntil(
    async () => {
      return await browser.execute(() => {
        return !!document.querySelector('[data-testid="notebook-toolbar"]');
      });
    },
    {
      timeout: 15000,
      interval: 300,
      timeoutMsg: "App not ready — toolbar not found within 15s",
    },
  );
}

/**
 * Wait for the notebook to finish initial Automerge sync and render cells.
 *
 * Checks the `data-notebook-synced` attribute set by NotebookView when
 * `isLoading` is false and `cells.length > 0`. This is the canonical
 * signal that the doc has synced and cells are in the DOM — no arbitrary
 * timeouts or cell-count polling needed.
 */
export async function waitForNotebookSynced(timeout = 15000) {
  await waitForAppReady();
  await browser.waitUntil(
    async () => {
      return await browser.execute(() => {
        const el = document.querySelector("[data-notebook-synced]");
        return el?.getAttribute("data-notebook-synced") === "true";
      });
    },
    {
      timeout,
      interval: 300,
      timeoutMsg: `Notebook not synced within ${timeout / 1000}s`,
    },
  );
}

/**
 * Wait for the daemon session-control runtime gate to match the app's
 * execution readiness check.
 *
 * Kernel toolbar state comes from RuntimeStateDoc; ExecuteCell is still
 * fail-closed until SessionControl reports runtime_state=ready.
 */
export async function waitForSessionReady(timeout = 30000) {
  await waitForNotebookSynced(timeout);
  try {
    await browser.waitUntil(
      async () => {
        return await browser.execute(() => {
          const el = document.querySelector("[data-session-ready]");
          return el?.getAttribute("data-session-ready") === "true";
        });
      },
      {
        timeout,
        interval: 300,
        timeoutMsg: `Session runtime not ready within ${timeout / 1000}s`,
      },
    );
  } catch (error) {
    const state = await browser.execute(() => {
      const el = document.querySelector("[data-session-runtime-state]");
      return el?.getAttribute("data-session-runtime-state") ?? "missing";
    });
    throw new Error(
      error instanceof Error
        ? `${error.message} (state=${state})`
        : `Session runtime not ready within ${timeout / 1000}s (state=${state})`,
    );
  }
}

/**
 * Wait for a specific number of code cells to be loaded.
 * Use this in fixture tests where the notebook has pre-populated cells.
 */
export async function waitForCodeCells(expectedCount, timeout = 15000) {
  await waitForAppReady();
  await browser.waitUntil(
    async () => {
      return await browser.execute((count) => {
        const cells = document.querySelectorAll('[data-cell-type="code"]');
        return cells.length >= count;
      }, expectedCount);
    },
    {
      timeout,
      interval: 300,
      timeoutMsg: `Expected ${expectedCount} code cells but they did not load within ${timeout / 1000}s`,
    },
  );
}

/**
 * Wait for the kernel to reach idle or busy state.
 * Use this in specs that execute code — replaces both the 5000ms before()
 * pause AND the first kernel startup wait.
 */
export async function waitForKernelReady(timeout = 60000) {
  await waitForAppReady();
  await browser.waitUntil(
    async () => {
      const text = await getKernelStatus();
      return text === "idle" || text === "busy";
    },
    { timeout, interval: 200, timeoutMsg: "Kernel not ready" },
  );
}

/**
 * Find the first code cell and execute it.
 * Assumes the cell already has code (pre-populated in fixture notebooks).
 * Returns the cell element for further assertions.
 */
export async function executeFirstCell() {
  // Wait for the notebook to finish Automerge sync and render cells.
  await waitForNotebookSynced();

  const codeCell = await $('[data-cell-type="code"]');
  await codeCell.waitForExist({ timeout: 5000 });

  // Prefer explicit execute button click: more reliable than Shift+Enter in WRY.
  const executeButton = await codeCell.$('[data-testid="execute-button"]');
  if (await executeButton.isExisting()) {
    await executeButton.waitForClickable({ timeout: 5000 });
    await executeButton.click();
    return codeCell;
  }

  // Fallback for older UI variants that may not expose the execute button.
  const editor = await codeCell.$('.cm-content[contenteditable="true"]');
  await editor.waitForExist({ timeout: 5000 });
  await editor.click();
  await browser.pause(200);
  await browser.keys([MOD_KEY, "a"]);
  await browser.pause(100);
  await browser.keys(["ArrowRight"]);
  await browser.pause(100);
  await browser.keys(["Shift", "Enter"]);
  return codeCell;
}

/**
 * Wait for stream output to appear in a cell.
 * Returns the output text.
 *
 * Note: In daemon mode, the DOM may re-render after trust approval,
 * so we use a global selector if the cell-scoped one fails.
 */
export async function waitForCellOutput(cell, timeout = 120000) {
  const outputSelectors = [
    '[data-slot="ansi-stream-output"]',
    '[data-slot="ansi-error-output"]',
    '[data-slot="output-item"]',
  ];

  await browser.waitUntil(
    async () => {
      // Try cell-scoped selectors first.
      for (const selector of outputSelectors) {
        try {
          const output = await cell.$(selector);
          if (await output.isExisting()) {
            return true;
          }
        } catch {
          // Cell reference may be stale, continue to global check.
        }
      }

      // Fall back to global selectors (any output on page).
      for (const selector of outputSelectors) {
        const globalOutput = await $(selector);
        if (await globalOutput.isExisting()) {
          return true;
        }
      }

      return false;
    },
    {
      timeout,
      timeoutMsg: `No output appeared within ${timeout / 1000}s`,
      interval: 500,
    },
  );

  // Try cell-scoped first, then fall back to global.
  for (const selector of outputSelectors) {
    try {
      const cellOutput = await cell.$(selector);
      if (await cellOutput.isExisting()) {
        return await cellOutput.getText();
      }
    } catch {
      // Cell reference stale, use global check below.
    }
  }

  for (const selector of outputSelectors) {
    const globalOutput = await $(selector);
    if (await globalOutput.isExisting()) {
      return await globalOutput.getText();
    }
  }

  return "";
}

/**
 * Wait for a cell output to satisfy a predicate.
 * Returns the matching output text.
 */
export async function waitForCellOutputMatching(
  cell,
  predicate,
  timeout = 120000,
) {
  let matched = "";
  await browser.waitUntil(
    async () => {
      let text = "";
      try {
        text = await waitForCellOutput(cell, 1000);
      } catch {
        return false;
      }
      if (predicate(text)) {
        matched = text;
        return true;
      }
      return false;
    },
    {
      timeout,
      timeoutMsg: `Matching output did not appear within ${timeout / 1000}s`,
      interval: 500,
    },
  );
  return matched;
}

/**
 * Wait for stream output containing specific text.
 * Returns the full output text.
 */
export async function waitForOutputContaining(
  cell,
  expectedText,
  timeout = 120000,
) {
  await browser.waitUntil(
    async () => {
      const streamOutput = await cell.$('[data-slot="ansi-stream-output"]');
      if (!(await streamOutput.isExisting())) {
        return false;
      }
      const text = await streamOutput.getText();
      return text.includes(expectedText);
    },
    {
      timeout,
      timeoutMsg: `Output "${expectedText}" did not appear within ${timeout / 1000}s`,
      interval: 500,
    },
  );

  return await cell.$('[data-slot="ansi-stream-output"]').getText();
}

/**
 * Wait for error output to appear in a cell.
 * Returns the error text.
 */
export async function waitForErrorOutput(cell, timeout = 30000) {
  await browser.waitUntil(
    async () => {
      const errorOutput = await cell.$('[data-slot="ansi-error-output"]');
      return await errorOutput.isExisting();
    },
    {
      timeout,
      timeoutMsg: `Error output did not appear within ${timeout / 1000}s`,
      interval: 500,
    },
  );

  return await cell.$('[data-slot="ansi-error-output"]').getText();
}

/**
 * Wait for the trust dialog to appear and click "Trust & Install".
 * Call this after executing a cell in an untrusted notebook with inline deps.
 * The trust dialog appears because the kernel won't start until deps are approved.
 *
 * In daemon mode, the trust check may be bypassed (daemon handles trust differently),
 * so this function will return false if the dialog doesn't appear within the timeout.
 *
 * @param timeout Max time to wait for the dialog (default 15s)
 * @returns true if dialog was approved, false if dialog didn't appear
 */
export async function approveTrustDialog(timeout = 15000) {
  const dialog = await $('[data-testid="trust-dialog"]');

  // Try to wait for dialog, but don't fail if it doesn't appear (daemon mode may skip trust)
  try {
    await dialog.waitForExist({ timeout });
  } catch {
    // Dialog didn't appear - daemon mode may have bypassed trust
    console.log("Trust dialog did not appear (may be daemon mode)");
    return false;
  }

  const approveButton = await $('[data-testid="trust-approve-button"]');
  // daemon:ready can briefly set loading=true, disabling the button — wait generously
  await approveButton.waitForEnabled({ timeout: 30000 });
  await approveButton.waitForClickable({ timeout: 5000 });
  await approveButton.click();

  // Wait for dialog to close — approveTrust() does two daemon IPCs
  // (approve_notebook_trust + checkTrust re-verify) which can take 10-20s
  // when the daemon is busy with pool warming or env creation.
  await browser.waitUntil(
    async () => {
      return !(await dialog.isExisting());
    },
    { timeout: 30000, interval: 300, timeoutMsg: "Trust dialog did not close" },
  );

  return true;
}

/**
 * Wait for kernel ready while handling trust approval if needed.
 *
 * For untrusted notebooks, the kernel won't auto-launch — the user must
 * trigger trust approval first. This function:
 * 1. Checks if the kernel is already ready (trusted notebook case)
 * 2. If not, tries to trigger trust via the UntrustedBanner or execute button
 * 3. Approves the trust dialog if it appears
 * 4. Waits for the kernel to become ready after trust approval
 *
 * @param timeout Max time to wait for kernel ready
 * @returns true if trust dialog was approved, false if it didn't appear
 */
export async function waitForKernelReadyWithTrust(timeout = 300000) {
  await waitForAppReady();

  // Capture full app state for diagnostics
  const initialStatus = await getKernelStatus();
  const bannerExists = await browser.execute(() => {
    return !!document.querySelector(
      '[data-testid="review-dependencies-button"]',
    );
  });
  const trustDialogExists = await browser.execute(() => {
    return !!document.querySelector('[data-testid="trust-dialog"]');
  });
  console.log(
    `[trust] Initial state: kernel=${initialStatus}, banner=${bannerExists}, dialog=${trustDialogExists}`,
  );

  // Check if kernel is already ready (trusted notebook, or daemon auto-trust)
  if (initialStatus === "idle" || initialStatus === "busy") {
    console.log("[trust] Kernel already ready, no trust needed");
    return false;
  }

  // Try to trigger the trust dialog via the banner "Review Dependencies" button
  let trustTriggered = false;
  try {
    const reviewButton = await $('[data-testid="review-dependencies-button"]');
    await reviewButton.waitForExist({ timeout: 30000 });
    await reviewButton.waitForClickable({ timeout: 5000 });
    await reviewButton.click();
    trustTriggered = true;
    console.log("[trust] Triggered trust dialog via banner");
  } catch (e) {
    console.log(`[trust] Banner not found after 30s: ${e.message}`);
  }

  // Fallback: try clicking execute to trigger trust check
  if (!trustTriggered) {
    try {
      const codeCell = await $('[data-cell-type="code"]');
      await codeCell.waitForExist({ timeout: 10000 });
      const executeButton = await codeCell.$('[data-testid="execute-button"]');
      await executeButton.waitForClickable({ timeout: 10000 });
      await executeButton.click();
      trustTriggered = true;
      console.log("[trust] Triggered trust dialog via execute");
    } catch (e) {
      console.log(`[trust] Execute fallback also failed: ${e.message}`);
    }
  }

  // Log state after trigger attempt
  const postTriggerStatus = await getKernelStatus();
  const postTriggerDialog = await browser.execute(() => {
    return !!document.querySelector('[data-testid="trust-dialog"]');
  });
  console.log(
    `[trust] After trigger: kernel=${postTriggerStatus}, dialog=${postTriggerDialog}`,
  );

  // Try to approve the trust dialog if it appears
  let trustApproved = false;
  try {
    const dialog = await $('[data-testid="trust-dialog"]');
    await dialog.waitForExist({ timeout: 30000 });
    console.log("[trust] Trust dialog found, approving...");
    const approveButton = await $('[data-testid="trust-approve-button"]');
    await approveButton.waitForEnabled({ timeout: 30000 });
    await approveButton.waitForClickable({ timeout: 5000 });
    await approveButton.click();
    await browser.waitUntil(async () => !(await dialog.isExisting()), {
      timeout: 30000,
      interval: 300,
    });
    trustApproved = true;
    console.log("[trust] Trust dialog approved successfully");
  } catch (e) {
    console.log(`[trust] Trust dialog not approved: ${e.message}`);
  }

  // Log kernel status periodically during the wait
  const startTime = Date.now();
  let lastLog = 0;
  await browser.waitUntil(
    async () => {
      const status = await getKernelStatus();
      const elapsed = Math.round((Date.now() - startTime) / 1000);
      if (elapsed - lastLog >= 30) {
        console.log(
          `[trust] Waiting for kernel: status=${status} (${elapsed}s)`,
        );
        lastLog = elapsed;
      }
      return status === "idle" || status === "busy";
    },
    {
      timeout,
      interval: 500,
      timeoutMsg: `Kernel not ready within ${timeout / 1000}s (trustApproved=${trustApproved})`,
    },
  );

  const finalStatus = await getKernelStatus();
  console.log(
    `[trust] Kernel ready: status=${finalStatus}, trustApproved=${trustApproved}`,
  );
  return trustApproved;
}

/**
 * Get the current kernel status text from the toolbar.
 */
export async function getKernelStatus() {
  return await browser.execute(() => {
    // Prefer the data-kernel-status attribute (added in #1041) — it reflects
    // the raw status enum value and doesn't depend on DOM text rendering.
    const statusEl = document.querySelector('[data-testid="kernel-status"]');
    if (statusEl) {
      const attr = statusEl.getAttribute("data-kernel-status");
      if (attr) return attr.trim().toLowerCase();
    }
    // Fallback: read the visible status text from the toolbar
    const el = document.querySelector(
      '[data-testid="notebook-toolbar"] .capitalize',
    );
    return el ? el.textContent.trim().toLowerCase() : "";
  });
}

/**
 * Wait for the kernel to reach a specific status.
 */
export async function waitForKernelStatus(status, timeout = 30000) {
  await browser.waitUntil(
    async () => {
      const current = await getKernelStatus();
      return current === status;
    },
    {
      timeout,
      interval: 300,
      timeoutMsg: `Kernel did not reach "${status}" status within ${timeout / 1000}s`,
    },
  );
}

/**
 * Type text character by character with delay.
 * Use this when typing into CodeMirror editors where bulk input may drop keys.
 *
 * NOTE: This uses browser.keys() which dispatches JS keyboard events.
 * Some WebDriver implementations (e.g. tauri-plugin-webdriver) dispatch
 * synthetic events that CodeMirror ignores. Prefer setCellSource() for
 * reliable text insertion into CodeMirror editors.
 */
export async function typeSlowly(text, delay = 30) {
  for (const char of text) {
    // Newline characters must be sent as the Enter key — browser.keys('\n')
    // doesn't produce Enter in all WebDriver environments (e.g. Linux/WRY).
    if (char === "\n") {
      await browser.keys("Enter");
    } else {
      await browser.keys(char);
    }
    await browser.pause(delay);
  }
}

/**
 * Set the source of a code cell via CodeMirror's dispatch API.
 *
 * This bypasses WebDriver sendKeys/keyboard events entirely, using
 * browser.execute() to call CodeMirror's transaction API directly.
 * This is the reliable way to set cell content — synthetic keyboard
 * events from JS-based WebDriver implementations don't flow through
 * CodeMirror's input pipeline.
 *
 * @param {WebdriverIO.Element} codeCell - The cell element ([data-cell-type="code"])
 * @param {string} source - The code to set
 */
export async function setCellSource(codeCell, source) {
  await browser.execute(
    (cellEl, text) => {
      // Find the CodeMirror editor within the cell
      const cmContent = cellEl.querySelector(".cm-content[contenteditable]");
      if (!cmContent) throw new Error("No CodeMirror editor found in cell");

      // Get the CodeMirror EditorView instance from the DOM.
      // CM6 stores the view on .cmTile.view (not .cmView.view).
      const cmEditor = cmContent.cmTile?.view;
      if (!cmEditor) throw new Error("No CodeMirror view found");

      // Replace the entire document content via a transaction
      cmEditor.dispatch({
        changes: {
          from: 0,
          to: cmEditor.state.doc.length,
          insert: text,
        },
      });
    },
    codeCell,
    source,
  );
  // Brief pause for the CRDT to sync the source to the daemon
  await browser.pause(300);
}

/**
 * Find a button by trying multiple selectors. Returns the first match, or null.
 */
export async function findButton(labelPatterns) {
  for (const pattern of labelPatterns) {
    try {
      const button = await $(pattern);
      if (await button.isExisting()) {
        return button;
      }
    } catch (_e) {}
  }
  return null;
}

/**
 * Set up a code cell for typing: find (or create) a code cell,
 * focus its editor, and select all content.
 * Returns the cell element.
 */
export async function setupCodeCell() {
  let codeCell = await $('[data-cell-type="code"]');
  const cellExists = await codeCell.isExisting();

  if (!cellExists) {
    const addCodeButton = await $('[data-testid="add-code-cell-button"]');
    await addCodeButton.waitForClickable({ timeout: 5000 });
    await addCodeButton.click();
    await browser.pause(500);

    codeCell = await $('[data-cell-type="code"]');
    await codeCell.waitForExist({ timeout: 5000 });
  }

  const editor = await codeCell.$('.cm-content[contenteditable="true"]');
  await editor.waitForExist({ timeout: 5000 });
  await editor.click();
  await browser.pause(200);

  // Select all to prepare for replacement
  await browser.keys([MOD_KEY, "a"]);
  await browser.pause(100);

  return codeCell;
}

/**
 * Check if a Python executable path is from a UV-managed environment.
 * Works for both local mode (runt/envs) and daemon mode (runtimed-uv).
 */
export function isUvManagedEnv(path) {
  return path.includes("runt/envs") || path.includes("runtimed-uv");
}

/**
 * Check if a Python executable path is from a Conda-managed environment.
 * Works for both local mode (runt/conda-envs) and daemon mode (runtimed-conda).
 */
export function isCondaManagedEnv(path) {
  return path.includes("runt/conda-envs") || path.includes("runtimed-conda");
}

/**
 * Check if a Python executable path is from a system Python with ipykernel.
 * In daemon mode, kernels may use system Python (pyenv, homebrew, etc.)
 * instead of prewarmed environments. This is still "managed" by runt.
 */
export function isSystemPythonEnv(path) {
  // pyenv, homebrew, system Python, or standard Python paths
  return (
    path.includes(".pyenv") ||
    path.includes("/opt/homebrew") ||
    path.includes("/usr/local") ||
    path.includes("/usr/bin/python")
  );
}

/**
 * Check if a Python executable path is from any runt-managed environment.
 * In local mode: prewarmed UV or Conda environments
 * In daemon mode: prewarmed envs OR system Python managed by daemon
 */
export function isManagedEnv(path) {
  return (
    isUvManagedEnv(path) || isCondaManagedEnv(path) || isSystemPythonEnv(path)
  );
}
