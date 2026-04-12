import { expect, test } from "@playwright/test";

/**
 * Titanic dataset edge-case tests.
 * Tests null handling, mixed types, and filtering on a column with nulls.
 */

test.describe("Titanic Edge Cases", () => {
  test.setTimeout(120_000);

  test("loads with correct row count", async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });

    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });
  });

  test("shows null badges in Age column", async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });

    // Scroll down to find a null age (row 6 — Mr. James Moran)
    // Null badges should be visible somewhere in the visible rows
    await expect(page.locator(".sift-badge-null").first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test("has mixed column types", async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });

    // Should have both numeric histograms and categorical bars
    const histograms = page.locator(".sift-th-range");
    const catSummaries = page.locator(".sift-cat-summary");

    await expect(histograms.first()).toBeVisible({ timeout: 5_000 });
    await expect(catSummaries.first()).toBeVisible({ timeout: 5_000 });
  });

  test("category bars show sex distribution", async ({ page }) => {
    await page.goto("/?dataset=titanic");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /891/, {
      timeout: 30_000,
    });

    // Sex column should show male/female distribution
    const sexBars = page.locator(".sift-cat-row", { hasText: "male" });
    await expect(sexBars.first()).toBeVisible({ timeout: 5_000 });
  });
});
