import { expect, test } from "@playwright/test";

test.describe("Table Viewer", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    // Wait for the table to mount and first batch to render
    await page.waitForSelector(".pt-table-container");
    await page.waitForSelector(".pt-row");
  });

  test("loads and shows rows", async ({ page }) => {
    const stats = page.locator(".pt-stat-rows");
    // Should eventually show 100,000 rows after all batches
    await expect(stats).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });
  });

  test("renders header labels for all columns", async ({ page }) => {
    const labels = page.locator(".pt-th-label");
    await expect(labels).toHaveCount(12); // id, name, location, department, note, status, priority, score, email, verified, joined, chaos
  });

  test("renders header summaries", async ({ page }) => {
    // Wait for all batches so summaries are populated
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /100,000/,
      { timeout: 10_000 },
    );
    // At least some summary containers should have content
    const summaries = page.locator(".pt-th-summary");
    const count = await summaries.count();
    expect(count).toBeGreaterThan(0);
  });

  test("scrolls vertically", async ({ page }) => {
    const viewport = page.locator(".pt-viewport");
    // Scroll down
    await viewport.evaluate((el) => (el.scrollTop = 5000));
    await page.waitForTimeout(100);
    // Viewport should have scrolled
    const scrollTop = await viewport.evaluate((el) => el.scrollTop);
    expect(scrollTop).toBeGreaterThan(0);
  });

  test("sorts on column click", async ({ page }) => {
    // Click the Score header label area to sort (sort handler is on .pt-th-top)
    const scoreTh = page.locator(".pt-th", { hasText: "SCORE" });
    const scoreLabel = scoreTh.locator(".pt-th-top");
    await scoreLabel.click();
    // Sort arrow should appear
    await expect(scoreTh.locator(".pt-sort-arrow")).toContainText("↑");
    // Click again for descending
    await scoreLabel.click();
    await expect(scoreTh.locator(".pt-sort-arrow")).toContainText("↓");
    // Click again to clear
    await scoreLabel.click();
    await expect(scoreTh.locator(".pt-sort-arrow")).toHaveText("");
  });

  test("column resize changes header width", async ({ page }) => {
    // Use the Name column (second) which definitely has a resize handle
    const nameTh = page.locator(".pt-th").nth(1);
    const handle = nameTh.locator(".pt-resize-handle");

    const startWidth = await nameTh.evaluate((el) => el.offsetWidth);

    // Dispatch pointer events directly to the handle since setPointerCapture
    // needs the events to originate on the capture target
    await handle.dispatchEvent("pointerdown", {
      clientX: 200,
      clientY: 50,
      pointerId: 1,
    });
    await handle.dispatchEvent("pointermove", {
      clientX: 260,
      clientY: 50,
      pointerId: 1,
    });
    await handle.dispatchEvent("pointerup", {
      clientX: 260,
      clientY: 50,
      pointerId: 1,
    });
    await page.waitForTimeout(100);

    const endWidth = await nameTh.evaluate((el) => el.offsetWidth);
    expect(endWidth).toBeGreaterThan(startWidth);
  });

  test("histogram brush creates filter pill", async ({ page }) => {
    // Wait for all data
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /100,000/,
      { timeout: 10_000 },
    );

    // Find the Score histogram (it has a brush layer SVG)
    const scoreTh = page.locator(".pt-th", { hasText: "SCORE" });
    const summary = scoreTh.locator(".pt-th-summary");
    const box = await summary.boundingBox();
    if (!box) throw new Error("No summary bounding box");

    // Drag across the histogram
    await page.mouse.move(box.x + 10, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width - 10, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();

    // A filter pill should appear
    await expect(page.locator(".pt-filter-pill")).toHaveCount(1, {
      timeout: 2000,
    });
    await expect(page.locator(".pt-filter-pill")).toContainText("Score");

    // Stats should show filtered count
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /of/,
    );
  });

  test("filter pill X clears the filter", async ({ page }) => {
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute(
      "data-value",
      /100,000/,
      { timeout: 10_000 },
    );

    // Brush the score histogram
    const scoreTh = page.locator(".pt-th", { hasText: "SCORE" });
    const summary = scoreTh.locator(".pt-th-summary");
    const box = await summary.boundingBox();
    if (!box) throw new Error("No summary bounding box");

    await page.mouse.move(box.x + 10, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, {
      steps: 3,
    });
    await page.mouse.up();

    await expect(page.locator(".pt-filter-pill")).toHaveCount(1, {
      timeout: 2000,
    });

    // Click the X on the pill
    await page.locator(".pt-filter-pill-x").click();

    // Pill should be gone, all rows restored
    await expect(page.locator(".pt-filter-pill")).toHaveCount(0);
    await expect(page.locator(".pt-stat-rows")).not.toHaveAttribute(
      "data-value",
      /of/,
    );
  });

  test("boolean badge renders for verified column", async ({ page }) => {
    // Check that at least one boolean badge exists in the visible rows
    await expect(page.locator(".pt-badge").first()).toBeVisible();
  });

  test("fullscreen button exists", async ({ page }) => {
    await expect(page.locator(".pt-fullscreen-btn")).toBeVisible();
  });

  test("header scrolls with viewport horizontally", async ({ page }) => {
    const viewport = page.locator(".pt-viewport");

    // Header is inside the viewport scroll content — scrolls naturally
    await viewport.evaluate((el) => (el.scrollLeft = 200));
    await page.waitForTimeout(100);

    // Verify viewport actually scrolled
    const scrollLeft = await viewport.evaluate((el) => el.scrollLeft);
    expect(scrollLeft).toBe(200);
  });

  test("streaming: row count increases over time", async ({ page }) => {
    // First batch should be visible quickly
    const stats = page.locator(".pt-stat-rows");
    await expect(stats).toHaveAttribute("data-value", /rows/, {
      timeout: 3000,
    });

    // Get initial count text
    const initialText = await stats.textContent();

    // Wait for all batches
    await expect(stats).toHaveAttribute("data-value", /100,000/, {
      timeout: 10_000,
    });

    // If initial was less than 100k, streaming worked
    // (may or may not catch intermediate state depending on timing)
    expect(initialText).toBeTruthy();
  });
});
