import { expect, test } from "@playwright/test";

/**
 * Heart Failure dataset — numeric-heavy E2E tests.
 * 299 rows, 13 columns. Mostly numeric with a few boolean-like columns
 * (DEATH_EVENT, anaemia, diabetes, high_blood_pressure, sex, smoking).
 * Tests code paths for all-numeric summaries, histograms, and sorting.
 */

test.describe("Heart Failure (numeric-heavy)", () => {
  test.setTimeout(120_000);

  const waitForData = async (page: import("@playwright/test").Page) => {
    await page.goto("/?dataset=heart-failure");
    await page.waitForSelector(".sift-table-container", { timeout: 90_000 });
    await page.waitForFunction(() => document.body.innerText.includes("299 rows"), null, {
      timeout: 30_000,
    });
  };

  test("row count shows 299 rows", async ({ page }) => {
    await waitForData(page);
    await expect(page.locator(".sift-stat-rows")).toHaveAttribute("data-value", /299/);
  });

  test("all numeric columns get histograms (SVG charts)", async ({ page }) => {
    await waitForData(page);

    // Numeric columns should have SVG histogram charts in their summary area.
    // Heart-failure has columns like age, creatinine_phosphokinase, ejection_fraction,
    // platelets, serum_creatinine, serum_sodium, time — all numeric.
    const histogramSvgs = page.locator(".sift-th-summary svg");
    const count = await histogramSvgs.count();

    // At least 5 numeric histograms should be present (the dataset has ~7 pure numeric columns)
    expect(count).toBeGreaterThanOrEqual(5);
  });

  test("numeric sort ascending — first value ≤ last value", async ({ page }) => {
    await waitForData(page);

    // Click the "age" column header to sort ascending
    const ageTh = page.locator(".sift-th", { hasText: "age" });
    const ageTop = ageTh.locator(".sift-th-top");
    await ageTop.click();

    // Verify sort arrow shows ascending
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↑");

    // Wait a moment for sort to apply
    await page.waitForTimeout(200);

    // Read the first and last visible cell values in the age column
    const ageCells = page.locator(".sift-row .sift-cell:first-child");
    const firstText = await ageCells.first().textContent();
    const allCells = await ageCells.all();
    const lastText = await allCells[allCells.length - 1].textContent();

    const firstVal = parseFloat(firstText ?? "");
    const lastVal = parseFloat(lastText ?? "");

    expect(firstVal).not.toBeNaN();
    expect(lastVal).not.toBeNaN();
    expect(firstVal).toBeLessThanOrEqual(lastVal);
  });

  test("numeric sort descending — first value ≥ last value", async ({ page }) => {
    await waitForData(page);

    // Click "age" column header twice: first click = asc, second = desc
    const ageTh = page.locator(".sift-th", { hasText: "age" });
    const ageTop = ageTh.locator(".sift-th-top");
    await ageTop.click();
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↑");
    await ageTop.click();
    await expect(ageTh.locator(".sift-sort-arrow")).toContainText("↓");

    await page.waitForTimeout(200);

    const ageCells = page.locator(".sift-row .sift-cell:first-child");
    const firstText = await ageCells.first().textContent();
    const allCells = await ageCells.all();
    const lastText = await allCells[allCells.length - 1].textContent();

    const firstVal = parseFloat(firstText ?? "");
    const lastVal = parseFloat(lastText ?? "");

    expect(firstVal).not.toBeNaN();
    expect(lastVal).not.toBeNaN();
    expect(firstVal).toBeGreaterThanOrEqual(lastVal);
  });

  test("column widths are reasonable (60–400px)", async ({ page }) => {
    await waitForData(page);

    const headers = page.locator(".sift-th");
    const count = await headers.count();
    expect(count).toBeGreaterThan(0);

    for (let i = 0; i < count; i++) {
      const width = await headers.nth(i).evaluate((el) => el.offsetWidth);
      expect(width).toBeGreaterThanOrEqual(60);
      expect(width).toBeLessThanOrEqual(400);
    }
  });

  test("histogram range labels show min – max", async ({ page }) => {
    await waitForData(page);

    // Each numeric column should have a .sift-th-range element with "min – max" text
    const rangeLabels = page.locator(".sift-th-range");
    const count = await rangeLabels.count();

    // At least some numeric columns should show range labels
    expect(count).toBeGreaterThanOrEqual(3);

    // Each range label should contain the en-dash separator
    for (let i = 0; i < count; i++) {
      const text = await rangeLabels.nth(i).textContent();
      expect(text).toMatch(/–/);
    }
  });

  test("DEATH_EVENT column shows boolean ratio bar", async ({ page }) => {
    await waitForData(page);

    // DEATH_EVENT is a boolean-like column (0/1) — should render as a ratio bar
    const deathTh = page.locator(".sift-th", { hasText: "DEATH_EVENT" });
    await expect(deathTh).toBeVisible({ timeout: 5_000 });

    // Boolean columns render a .sift-bool-bar instead of a histogram SVG
    const boolBar = deathTh.locator(".sift-bool-bar");
    await expect(boolBar).toBeVisible({ timeout: 5_000 });
  });
});
