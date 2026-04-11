import { expect, type Page, test } from "@playwright/test";

/**
 * Tests for column type casting and undo.
 * Validates the WASM original_columns save/restore from PR #75.
 */

async function openColumnMenu(page: Page, columnLabel: string) {
  const th = page.locator(".pt-th").filter({
    has: page.locator(".pt-th-label", {
      hasText: new RegExp(`^${columnLabel}$`),
    }),
  });
  await th.click({ button: "right" });
  await expect(page.locator(".fixed.z-50")).toBeVisible({ timeout: 2000 });
}

async function clickMenuItem(page: Page, text: string) {
  await page.locator(".fixed.z-50 button", { hasText: text }).click();
}

test.describe("Cast Column Undo (Titanic)", () => {
  test.setTimeout(120_000);

  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".pt-table-container", { timeout: 90_000 });
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });
  });

  test("cast Name to Number then back to Text restores values", async ({ page }) => {
    // Get the original first row's Name value
    const labels = await page.locator(".pt-th-label").allTextContents();
    const nameIdx = labels.indexOf("Name");
    expect(nameIdx).toBeGreaterThan(-1);

    const firstRow = page.locator(".pt-row").first();
    const nameCell = firstRow.locator(".pt-cell").nth(nameIdx);
    const originalName = await nameCell.textContent();
    expect(originalName).toBeTruthy();
    expect(originalName!.length).toBeGreaterThan(3); // Should be a real name

    // Cast Name to Number
    await openColumnMenu(page, "Name");
    await clickMenuItem(page, "Number");
    await page.waitForTimeout(500);

    // After casting to Number, the type icon should change to #
    const nameTh = page.locator(".pt-th").filter({
      has: page.locator(".pt-th-label", { hasText: /^Name$/ }),
    });
    await expect(nameTh.locator(".pt-type-icon")).toHaveText("#");

    // Now cast back to Text
    await openColumnMenu(page, "Name");
    await clickMenuItem(page, "Text");
    await page.waitForTimeout(500);

    // Type icon should be back to Aa
    await expect(nameTh.locator(".pt-type-icon")).toHaveText("Aa");

    // The original name value should be restored
    const restoredName = await nameCell.textContent();
    expect(restoredName).toBe(originalName);
  });

  test("cast Fare to Text then back to Number preserves values", async ({ page }) => {
    const labels = await page.locator(".pt-th-label").allTextContents();
    const fareIdx = labels.indexOf("Fare");
    expect(fareIdx).toBeGreaterThan(-1);

    const firstRow = page.locator(".pt-row").first();
    const fareCell = firstRow.locator(".pt-cell").nth(fareIdx);
    const originalFare = await fareCell.textContent();

    // Cast Fare to Text
    await openColumnMenu(page, "Fare");
    await clickMenuItem(page, "Text");
    await page.waitForTimeout(500);

    const fareTh = page.locator(".pt-th").filter({
      has: page.locator(".pt-th-label", { hasText: /^Fare$/ }),
    });
    await expect(fareTh.locator(".pt-type-icon")).toHaveText("Aa");

    // Cast back to Number
    await openColumnMenu(page, "Fare");
    await clickMenuItem(page, "Number");
    await page.waitForTimeout(500);

    await expect(fareTh.locator(".pt-type-icon")).toHaveText("#");

    // Value should be restored
    const restoredFare = await fareCell.textContent();
    expect(restoredFare).toBe(originalFare);
  });
});
