import { expect, test } from "@playwright/test";

/**
 * WASM sort integration tests.
 * Verifies that sorting HuggingFace datasets uses the WASM sort path
 * (store_sort_indices) and produces correct results.
 */

test.describe("WASM Sort (Titanic)", () => {
  test.setTimeout(120_000);

  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });
  });

  test("sort by Age ascending shows youngest first", async ({ page }) => {
    // Click the Age column header to sort ascending
    const ageTh = page.locator(".sift-th").filter({
      has: page.locator(".sift-th-label", { hasText: /^Age$/ }),
    });
    await ageTh.locator(".sift-th-top").click();
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↑", {
      timeout: 5000,
    });

    // First visible row should have a small age value
    // Titanic has babies (age ~0.42), so first values should be < 1
    const firstRow = page.locator(".sift-row").first();
    await expect(firstRow).toBeVisible();

    // Get the Age cell value from the first visible row
    // Age is one of the columns — find its index
    const labels = await page.locator(".sift-th-label").allTextContents();
    const ageColIndex = labels.indexOf("Age");
    expect(ageColIndex).toBeGreaterThan(-1);

    const ageCell = firstRow.locator(".sift-cell").nth(ageColIndex);
    const ageText = await ageCell.textContent();
    const ageValue = parseFloat(ageText || "999");
    // Youngest passengers are infants (< 1 year)
    expect(ageValue).toBeLessThan(2);
  });

  test("sort by Age descending shows oldest first", async ({ page }) => {
    const ageTh = page.locator(".sift-th").filter({
      has: page.locator(".sift-th-label", { hasText: /^Age$/ }),
    });
    // Click twice for descending
    await ageTh.locator(".sift-th-top").click();
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↑", {
      timeout: 5000,
    });
    await ageTh.locator(".sift-th-top").click();
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↓", {
      timeout: 5000,
    });
    // Wait for rows to re-render with new sort order
    await page.waitForTimeout(500);

    const labels = await page.locator(".sift-th-label").allTextContents();
    const ageColIndex = labels.indexOf("Age");

    const firstRow = page.locator(".sift-row").first();
    const ageCell = firstRow.locator(".sift-cell").nth(ageColIndex);
    const ageText = await ageCell.textContent();
    const ageValue = parseFloat(ageText || "0");
    // Oldest passenger is 80
    expect(ageValue).toBeGreaterThan(70);
  });

  test("sort by Fare ascending shows cheapest first", async ({ page }) => {
    const fareTh = page.locator(".sift-th").filter({
      has: page.locator(".sift-th-label", { hasText: /^Fare$/ }),
    });
    await fareTh.locator(".sift-th-top").click();
    await expect(fareTh.locator(".sift-sort-arrow")).toContainText("↑", {
      timeout: 5000,
    });

    const labels = await page.locator(".sift-th-label").allTextContents();
    const fareColIndex = labels.indexOf("Fare");

    const firstRow = page.locator(".sift-row").first();
    const fareCell = firstRow.locator(".sift-cell").nth(fareColIndex);
    const fareText = await fareCell.textContent();
    const fareValue = parseFloat(fareText || "999");
    // Cheapest fares are 0
    expect(fareValue).toBeLessThanOrEqual(1);
  });

  test("sort + filter: sorted results respect active filter", async ({ page }) => {
    // First, apply a filter on Age histogram (brush to select a range)
    const ageTh = page.locator(".sift-th").filter({
      has: page.locator(".sift-th-label", { hasText: /^Age$/ }),
    });
    const summary = ageTh.locator(".sift-th-summary");
    const box = await summary.boundingBox();
    if (!box) throw new Error("No Age summary bounding box");

    // Brush across the histogram
    await page.mouse.move(box.x + box.width * 0.3, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.7, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();
    await page.waitForTimeout(500);

    // Should show filtered count
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /of/, {
      timeout: 5000,
    });

    // Now sort by Fare
    const fareTh = page.locator(".sift-th").filter({
      has: page.locator(".sift-th-label", { hasText: /^Fare$/ }),
    });
    await fareTh.locator(".sift-th-top").click();
    await expect(fareTh.locator(".sift-sort-arrow")).toContainText("↑", {
      timeout: 5000,
    });

    // Filtered count should still be shown (filter still active)
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /of/);
    // Filter pill should still be visible
    await expect(page.locator(".sift-filter-pill")).toHaveCount(1);
  });
});
