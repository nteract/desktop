import { expect, test } from "@playwright/test";

/**
 * HuggingFace dataset loading tests.
 * These require network access and the nteract-predicate WASM module (4.3MB).
 * Generous timeouts account for WASM init + HF API + Parquet download.
 */

test.describe("HuggingFace Dataset Loading", () => {
  // 2 minutes per test — WASM init (4.3MB) + HF network on CI runners
  test.setTimeout(120_000);

  test("Heart Failure loads with boolean columns", async ({ page }) => {
    await page.goto("/?dataset=heart-failure");
    await page.waitForSelector(".pt-table-container", { timeout: 90_000 });

    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /299/,
      { timeout: 30_000 },
    );
    await expect(page.locator(".pt-badge").first()).toBeVisible({
      timeout: 5_000,
    });
  });

  test("Adult Census loads with many categorical columns", async ({ page }) => {
    await page.goto("/?dataset=adult-census");
    await page.waitForSelector(".pt-table-container", { timeout: 90_000 });

    // Wait for all row groups to load (progressive loading shows partial counts first)
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /3[0-9],/,
      { timeout: 60_000 },
    );

    await expect(page.locator(".pt-cat-summary").first()).toBeVisible({
      timeout: 5_000,
    });
  });
});
