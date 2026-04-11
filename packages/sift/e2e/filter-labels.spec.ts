import { expect, test } from "@playwright/test";

/**
 * Verifies that timestamp filter labels show formatted dates,
 * not raw epoch milliseconds.
 */

test.describe("Filter Labels", () => {
  test("timestamp filter label shows formatted date, not epoch", async ({
    page,
  }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".pt-table-container");
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /100,000/,
      { timeout: 10_000 },
    );

    // Find the Joined column (timestamp) — may need to scroll right
    const labels = await page.locator(".pt-th-label").allTextContents();
    const joinedIdx = labels.indexOf("Joined");
    expect(joinedIdx).toBeGreaterThan(-1);

    // Scroll the Joined column header into view
    const joinedTh = page.locator(".pt-th").nth(joinedIdx);
    await joinedTh.scrollIntoViewIfNeeded();
    await page.waitForTimeout(200);

    const summary = page.locator(".pt-th-summary").nth(joinedIdx);
    const box = await summary.boundingBox();
    if (!box) throw new Error("No Joined summary bounding box");

    // Brush across the histogram
    await page.mouse.move(box.x + box.width * 0.2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.6, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();
    await page.waitForTimeout(500);

    // A filter pill should appear with a formatted date range
    const pill = page.locator(".pt-filter-pill");
    await expect(pill).toHaveCount(1, { timeout: 2000 });
    const pillText = await pill.textContent();

    // Should NOT contain raw epoch numbers (large integers)
    expect(pillText).not.toMatch(/\d{10,}/);

    // Should contain date-like text (month abbreviations or slashes)
    expect(pillText).toMatch(/[A-Z][a-z]{2}/);

    // The filter line in the header should also be formatted
    const filterLine = page.locator(".pt-filter-line").first();
    if ((await filterLine.count()) > 0) {
      const lineText = await filterLine.textContent();
      expect(lineText).not.toMatch(/\d{10,}/);
    }
  });
});
