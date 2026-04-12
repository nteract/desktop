import { expect, test } from "@playwright/test";

/**
 * Mobile viewport tests.
 * Verifies the table works at phone-sized viewports.
 */

test.describe("Mobile Viewport", () => {
  test.use({ viewport: { width: 390, height: 844 } }); // iPhone 14

  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");
    await page.waitForSelector(".sift-row");
  });

  test("table renders and shows rows at mobile width", async ({ page }) => {
    const rows = page.locator(".sift-row");
    const count = await rows.count();
    expect(count).toBeGreaterThan(0);

    // Stats bar is visible
    await expect(page.locator(".sift-stat-rows")).toBeVisible();
  });

  test("header summaries render at narrow width", async ({ page }) => {
    // At least some summary containers should have content
    const summaries = page.locator(".sift-th-summary");
    const count = await summaries.count();
    expect(count).toBeGreaterThan(0);
  });

  test("horizontal scroll works", async ({ page }) => {
    const viewport = page.locator(".sift-viewport");

    // Scroll right
    await viewport.evaluate((el) => (el.scrollLeft = 200));
    await page.waitForTimeout(100);

    const scrollLeft = await viewport.evaluate((el) => el.scrollLeft);
    expect(scrollLeft).toBeGreaterThan(0);
  });

  test("first pinned column stays visible when scrolling", async ({ page }) => {
    const viewport = page.locator(".sift-viewport");
    const firstTh = page.locator(".sift-th").first();

    // Scroll right
    await viewport.evaluate((el) => (el.scrollLeft = 300));
    await page.waitForTimeout(100);

    // Pinned column should still be visible
    await expect(firstTh).toBeVisible();
  });

  test("dataset picker is usable", async ({ page }) => {
    const select = page.locator("#dataset-select");
    await expect(select).toBeVisible();

    // Should be able to interact with it
    const options = select.locator("option");
    const count = await options.count();
    expect(count).toBeGreaterThan(1);
  });

  test("fullscreen button is visible", async ({ page }) => {
    await expect(page.locator(".sift-fullscreen-btn")).toBeVisible();
  });

  test("sort works via column header tap", async ({ page }) => {
    // Tap a column header to sort (click simulates tap at mobile viewport)
    const firstSortable = page.locator(".sift-th-top").first();
    await firstSortable.click();

    // Sort arrow should appear
    const arrow = page.locator(".sift-sort-arrow").first();
    await expect(arrow).toContainText("↑", { timeout: 2000 });
  });
});
