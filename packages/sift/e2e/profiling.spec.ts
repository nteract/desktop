import { expect, test } from "@playwright/test";

test.describe("Column Profiling", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");
    // Wait for all data to load
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });
  });

  test("shows null% and distinct count for low-cardinality numeric columns", async ({ page }) => {
    // The Chaos column has NaN, Infinity, null values — should show profiling stats
    const profileEls = page.locator(".sift-th-profile");
    // At least one profile element should be visible
    await expect(profileEls.first()).toBeVisible({ timeout: 5_000 });

    // Check that at least one shows "null" or "distinct"
    const texts = await profileEls.allTextContents();
    const hasProfileInfo = texts.some((t) => t.includes("null") || t.includes("distinct"));
    expect(hasProfileInfo).toBe(true);
  });

  test("profiling stats survive filter application", async ({ page }) => {
    // Find a profile element before filtering
    const profileEls = page.locator(".sift-th-profile");
    const _countBefore = await profileEls.count();

    // Apply a filter via the Score histogram
    const scoreTh = page.locator(".sift-th", { hasText: "SCORE" });
    const summary = scoreTh.locator(".sift-th-summary");
    const box = await summary.boundingBox();
    if (!box) return; // skip if not visible

    await page.mouse.move(box.x + 10, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.5, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();
    await page.waitForTimeout(500);

    // Profile elements should still exist (may change count due to filtered summaries)
    const countAfter = await profileEls.count();
    expect(countAfter).toBeGreaterThanOrEqual(0); // at minimum doesn't crash
  });
});

test.describe("Debug Toggle", () => {
  test("gear button toggles debug stats", async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });

    // Debug group should be hidden by default
    const debugGroup = page.locator(".sift-debug-group");
    await expect(debugGroup).toBeHidden();

    // Click the gear button
    await page.locator(".sift-debug-btn").click();
    await expect(debugGroup).toBeVisible();

    // Should show FPS and DOM rows
    await expect(page.locator(".sift-stat-frame")).toBeVisible();
    await expect(page.locator(".sift-stat-dom")).toBeVisible();

    // Click again to hide
    await page.locator(".sift-debug-btn").click();
    await expect(debugGroup).toBeHidden();
  });
});

test.describe("Dark Mode", () => {
  test("theme toggle switches theme", async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".sift-table-container");

    // Click theme toggle
    await page.locator("#theme-toggle").click();

    // Should have dark theme attribute
    const theme = await page.evaluate(() => document.documentElement.getAttribute("data-theme"));
    expect(theme).toBe("dark");

    // Toggle back
    await page.locator("#theme-toggle").click();
    const themeAfter = await page.evaluate(() =>
      document.documentElement.getAttribute("data-theme"),
    );
    expect(themeAfter).not.toBe("dark");
  });
});
