import { expect, test } from "@playwright/test";

/**
 * Verifies that the first data row is not hidden behind the sticky header.
 * Regression test for: header grows when React summary charts mount async,
 * but rowPool.style.top wasn't updated (fixed via ResizeObserver in PR #70).
 */

test.describe("Header Overlap", () => {
  test("first row is visible below header on generated dataset", async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");
    await page.waitForSelector(".sift-row");

    // Wait for summaries to render (React async mount)
    await page.waitForTimeout(500);

    const header = page.locator(".sift-header");
    const firstRow = page.locator(".sift-row").first();

    const headerBox = await header.boundingBox();
    const rowBox = await firstRow.boundingBox();

    expect(headerBox).not.toBeNull();
    expect(rowBox).not.toBeNull();

    // First row's top edge should be at or below the header's bottom edge
    expect(rowBox!.y).toBeGreaterThanOrEqual(headerBox!.y + headerBox!.height - 1);
  });

  test("first row is visible below header on HF dataset", async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });

    // Wait for summaries to render
    await page.waitForTimeout(1000);

    const header = page.locator(".sift-header");
    const firstRow = page.locator(".sift-row").first();

    const headerBox = await header.boundingBox();
    const rowBox = await firstRow.boundingBox();

    expect(headerBox).not.toBeNull();
    expect(rowBox).not.toBeNull();

    // First row's top edge should be at or below the header's bottom edge
    expect(rowBox!.y).toBeGreaterThanOrEqual(headerBox!.y + headerBox!.height - 1);
  });
});
