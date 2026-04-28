/**
 * E2E Test: Run All Output Lifecycle
 *
 * Verifies that Run All clears all cells' stale outputs up front and
 * shows new outputs as each cell completes. Regression test for the
 * rapid ctrl-enter output loss bug (PR #1201).
 *
 * Fixture: 15-run-all-output-lifecycle.ipynb
 *   - Cell 1: sleep(2) + print (slow — keeps cell 2 queued)
 *   - Cell 2: print (fast)
 *   - Both cells have pre-existing stale outputs in the fixture
 *
 * Run with: cargo xtask e2e test-fixture \
 *   crates/notebook/fixtures/audit-test/15-run-all-output-lifecycle.ipynb \
 *   e2e/specs/run-all-output-lifecycle.spec.js
 */

import { browser } from "@wdio/globals";
import { waitForAppReady, waitForKernelReady, waitForNotebookSynced } from "../helpers.js";

async function outputText() {
  return await browser.execute(() =>
    Array.from(
      document.querySelectorAll(
        '[data-slot="ansi-stream-output"], [data-slot="ansi-error-output"], [data-slot="output-item"]',
      ),
    )
      .map((el) => el.textContent ?? "")
      .join("\n"),
  );
}

async function waitForOutputTextContaining(text, timeout = 30000) {
  await browser.waitUntil(async () => (await outputText()).includes(text), {
    timeout,
    interval: 500,
    timeoutMsg: `Output text did not contain "${text}" within ${timeout / 1000}s`,
  });
}

async function waitForOutputTextExcluding(texts, timeout = 5000) {
  await browser.waitUntil(
    async () => {
      const text = await outputText();
      return texts.every((needle) => !text.includes(needle));
    },
    {
      timeout,
      interval: 250,
      timeoutMsg: `Page text still contained one of: ${texts.join(", ")}`,
    },
  );
}

describe("Run All Output Lifecycle", () => {
  it("should load and reach idle", async () => {
    await waitForAppReady();
    await waitForKernelReady(300000);
    await waitForNotebookSynced();
  });

  it("Run All should clear stale outputs and show new results", async () => {
    const cells = await $$('[data-cell-type="code"]');
    expect(cells.length).toBeGreaterThanOrEqual(2);

    // Verify stale outputs are visible from the fixture. The renderer now
    // stores outputs out-of-band, so assert on rendered page text instead of
    // relying on a stale WebDriver cell element.
    await waitForOutputTextContaining("stale-output-1", 10000);
    await waitForOutputTextContaining("stale-output-2", 10000);

    // Click "Run All"
    const runAllButton = await $('[data-testid="run-all-button"]');
    await runAllButton.waitForClickable({ timeout: 5000 });
    await runAllButton.click();

    // Stale outputs should disappear before new execution results arrive.
    await waitForOutputTextExcluding(["stale-output-1", "stale-output-2"]);

    // Wait for both cells to complete.
    await waitForOutputTextContaining("cell-1-done", 30000);
    await waitForOutputTextContaining("cell-2-done", 30000);
  });
});
