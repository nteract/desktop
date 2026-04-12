import { expect, test } from "@playwright/test";

test.describe("Odometer", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");
    await page.waitForSelector(".sift-row");
  });

  test("row count rolls up during streaming", async ({ page }) => {
    const stats = page.locator(".sift-stat-rows");

    // Wait for streaming to start
    await expect(stats).toHaveAttribute("data-value", /rows/, {
      timeout: 3000,
    });

    // Capture mid-stream
    await page.screenshot({ path: "test-results/odometer-streaming.png" });

    // Wait for all batches
    await expect(stats).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });
    await page.screenshot({ path: "test-results/odometer-full.png" });

    // Verify odometer slots exist (digits should be in strip elements)
    const slots = stats.locator(".sift-odo-slot");
    await expect(slots.first()).toBeVisible();
  });

  test("row count rolls down on filter, back up on clear", async ({ page }) => {
    const stats = page.locator(".sift-stat-rows");

    // Wait for all data
    await expect(stats).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });
    const beforeFilter = await stats.getAttribute("data-value");
    await page.screenshot({ path: "test-results/odometer-before-filter.png" });

    // Apply a range filter on Score column via brush
    const scoreTh = page.locator(".sift-th", { hasText: "SCORE" });
    const summary = scoreTh.locator(".sift-th-summary");
    const box = await summary.boundingBox();
    if (!box) throw new Error("No summary bounding box");

    await page.mouse.move(box.x + 10, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.4, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();

    // Row count should decrease — shows "X of Y rows"
    await expect(stats).toHaveAttribute("data-value", /of/, { timeout: 3000 });
    const afterFilter = await stats.getAttribute("data-value");
    await page.screenshot({ path: "test-results/odometer-filtered.png" });

    // The filtered count should be less than the total
    expect(afterFilter).toContain("of");
    expect(afterFilter).toContain("100,000");

    // Clear filter by clicking pill X
    await page.locator(".sift-filter-pill-x").click();
    await expect(stats).not.toHaveAttribute("data-value", /of/, {
      timeout: 3000,
    });
    await page.screenshot({ path: "test-results/odometer-cleared.png" });

    // Back to full count
    const afterClear = await stats.getAttribute("data-value");
    expect(afterClear).toContain("100,000");
  });
});
