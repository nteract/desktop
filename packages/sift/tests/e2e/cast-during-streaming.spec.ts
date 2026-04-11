import { test, expect } from "@playwright/test";

test.describe("Cast during streaming", () => {
  test("context menu disables cast options while streaming", async ({ page }) => {
    await page.goto("/?dataset=spotify");

    // Wait for table to appear but NOT finish streaming
    await page.waitForSelector('[role="columnheader"]', { timeout: 30000 });
    await page.waitForTimeout(1000);

    // Verify still streaming (not at full 114k yet)
    const rowText = await page.evaluate(() => document.body.innerText);
    const notFullyLoaded = !rowText.includes("114,000 rows");

    if (notFullyLoaded) {
      // Right-click a column header
      const popHeader = page.locator('[role="columnheader"]').filter({ hasText: "popularity" });
      await popHeader.click({ button: "right" });
      await page.waitForTimeout(300);

      // Should see the "available after loading" message, not cast options
      await expect(page.getByText("Some operations hidden while loading")).toBeVisible();

      // Close menu
      await page.keyboard.press("Escape");
    }
  });

  test("cast works after streaming completes", async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (err) => errors.push(err.message));

    await page.goto("/?dataset=spotify");

    // Wait for full load
    await page.waitForFunction(() => document.body.innerText.includes("114,000 rows"), null, {
      timeout: 60000,
    });
    await page.waitForTimeout(500);

    // Right-click popularity
    const popHeader = page.locator('[role="columnheader"]').filter({ hasText: "popularity" });
    await popHeader.click({ button: "right" });
    await page.waitForTimeout(300);

    // Cast options should be visible now
    await expect(page.getByText("Text", { exact: true })).toBeVisible();

    // Cast to text
    await page.getByText("Text", { exact: true }).click();
    await page.waitForTimeout(1000);

    // Verify rows still visible
    const rowCount = await page.locator('[role="row"]').count();
    expect(rowCount).toBeGreaterThan(1);

    // No WASM crashes
    const wasmErrors = errors.filter(
      (e) => e.includes("unreachable") || e.includes("RuntimeError"),
    );
    expect(wasmErrors).toHaveLength(0);
  });
});
