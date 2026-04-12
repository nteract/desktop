import { expect, test } from "@playwright/test";

test.describe("Pinned column", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    // Wait for table to render
    await page.waitForSelector(".sift-table-container", { timeout: 15_000 });
    await page.waitForSelector(".sift-row", { timeout: 15_000 });
  });

  test("first column header has opaque background when scrolled", async ({ page }) => {
    const header = page.locator(".sift-th:first-child");
    await expect(header).toBeVisible();

    // Scroll the table right so content goes behind the pinned column
    const viewport = page.locator(".sift-viewport");
    await viewport.evaluate((el) => {
      el.scrollLeft = 300;
    });
    await page.waitForTimeout(200);

    // The pinned header should have a non-transparent background
    const bg = await header.evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(bg).not.toBe("rgba(0, 0, 0, 0)");
    expect(bg).not.toBe("transparent");
  });

  test("first column cells have opaque background when scrolled", async ({ page }) => {
    const firstCell = page.locator(".sift-row:first-child .sift-cell:first-child");
    await expect(firstCell).toBeVisible();

    const viewport = page.locator(".sift-viewport");
    await viewport.evaluate((el) => {
      el.scrollLeft = 300;
    });
    await page.waitForTimeout(200);

    const bg = await firstCell.evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(bg).not.toBe("rgba(0, 0, 0, 0)");
    expect(bg).not.toBe("transparent");
  });
});
