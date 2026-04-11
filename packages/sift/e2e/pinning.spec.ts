import { expect, type Page, test } from "@playwright/test";

/** Right-click a column header to open the context menu. */
async function openColumnMenu(page: Page, columnLabel: string) {
  const th = page.locator(".pt-th").filter({
    has: page.locator(".pt-th-label", {
      hasText: new RegExp(`^${columnLabel}$`),
    }),
  });
  await th.click({ button: "right" });
  // Wait for the context menu to appear
  await expect(page.locator(".fixed.z-50")).toBeVisible({ timeout: 2000 });
}

/** Click a menu item by text. */
async function clickMenuItem(page: Page, text: string) {
  await page.locator(".fixed.z-50 button", { hasText: text }).click();
}

test.describe("Column Pinning", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".pt-table-container");
    await page.waitForSelector(".pt-row");
  });

  test("first column (ID) is pinned by default", async ({ page }) => {
    const firstTh = page.locator(".pt-th").first();
    const position = await firstTh.evaluate((el) => el.style.position);
    expect(position).toBe("sticky");
  });

  test("pinned column header stays sticky when scrolling horizontally", async ({
    page,
  }) => {
    const viewport = page.locator(".pt-viewport");
    const firstTh = page.locator(".pt-th").first();

    // Scroll far to the right
    await viewport.evaluate((el) => (el.scrollLeft = 800));
    await page.waitForTimeout(100);

    // Pinned column should still be visible (sticky positioning)
    await expect(firstTh).toBeVisible();
    const left = await firstTh.evaluate((el) => el.style.left);
    expect(left).toBe("0px");
  });

  test("pin a column via context menu", async ({ page }) => {
    // Name column is not pinned initially
    const nameTh = page.locator(".pt-th").filter({ hasText: "Name" });
    const initialPos = await nameTh.evaluate((el) => el.style.position);
    expect(initialPos).not.toBe("sticky");

    // Right-click Name header and pin it
    await openColumnMenu(page, "Name");
    await clickMenuItem(page, "Pin column");
    await page.waitForTimeout(200);

    // Name should now be sticky
    const afterPos = await nameTh.evaluate((el) => el.style.position);
    expect(afterPos).toBe("sticky");
  });

  test("unpin a column via context menu", async ({ page }) => {
    // First column (ID) is pinned by default
    const idTh = page
      .locator(".pt-th")
      .filter({ has: page.locator(".pt-th-label", { hasText: /^ID$/ }) });
    const initialPos = await idTh.evaluate((el) => el.style.position);
    expect(initialPos).toBe("sticky");

    // Right-click and unpin
    await openColumnMenu(page, "ID");
    await clickMenuItem(page, "Unpin column");
    await page.waitForTimeout(200);

    // ID should no longer be sticky
    const afterPos = await idTh.evaluate((el) => el.style.position);
    expect(afterPos).not.toBe("sticky");
  });

  test("pinned column has shadow on last pinned column", async ({ page }) => {
    // ID is the only pinned column — it should have the shadow
    const firstTh = page.locator(".pt-th").first();
    const shadow = await firstTh.evaluate((el) => el.style.boxShadow);
    expect(shadow).toContain("pin-shadow");
  });

  test("pinned cells in rows also have sticky positioning", async ({
    page,
  }) => {
    const firstCell = page
      .locator(".pt-row")
      .first()
      .locator(".pt-cell")
      .first();
    const position = await firstCell.evaluate((el) => el.style.position);
    expect(position).toBe("sticky");
  });

  test("multiple pinned columns have correct left offsets", async ({
    page,
  }) => {
    // Pin the Name column (in addition to ID which is already pinned)
    await openColumnMenu(page, "Name");
    await clickMenuItem(page, "Pin column");
    await page.waitForTimeout(200);

    // After pinning, both ID and Name headers should be sticky
    const idTh = page
      .locator(".pt-th")
      .filter({ has: page.locator(".pt-th-label", { hasText: /^ID$/ }) });
    const nameTh = page.locator(".pt-th").filter({ hasText: "Name" });

    // ID should be at left: 0
    const idLeft = await idTh.evaluate((el) => el.style.left);
    expect(idLeft).toBe("0px");

    // Name should be offset by the width of the ID column
    const nameLeft = await nameTh.evaluate((el) => parseFloat(el.style.left));
    expect(nameLeft).toBeGreaterThan(0);

    // Only the last pinned column should have the shadow
    const idShadow = await idTh.evaluate((el) => el.style.boxShadow);
    const nameShadow = await nameTh.evaluate((el) => el.style.boxShadow);
    expect(idShadow).toBe("");
    expect(nameShadow).toContain("pin-shadow");
  });

  test("pinned columns move to front of visual order", async ({ page }) => {
    // Get the initial order of column labels
    const labelsBefore = await page.locator(".pt-th-label").allTextContents();

    // Pin the Score column (initially not first)
    await openColumnMenu(page, "Score");
    await clickMenuItem(page, "Pin column");
    await page.waitForTimeout(200);

    const labelsAfter = await page.locator(".pt-th-label").allTextContents();

    // Score should now be near the front (after ID which is pinned at index 0)
    const scoreIndexBefore = labelsBefore.indexOf("Score");
    const scoreIndexAfter = labelsAfter.indexOf("Score");
    expect(scoreIndexAfter).toBeLessThan(scoreIndexBefore);
  });

  test('keyboard shortcut "p" toggles pin on focused column', async ({
    page,
  }) => {
    // Focus the Name column header and press 'p' to pin
    const nameTh = page.locator(".pt-th").filter({ hasText: "Name" });
    await nameTh.focus();
    await nameTh.press("p");
    await page.waitForTimeout(200);

    // Name should now be sticky (pinned)
    const pinnedPos = await nameTh.evaluate((el) => el.style.position);
    expect(pinnedPos).toBe("sticky");

    // Press 'p' again to unpin
    await nameTh.focus();
    await nameTh.press("p");
    await page.waitForTimeout(200);

    const unpinnedPos = await nameTh.evaluate((el) => el.style.position);
    expect(unpinnedPos).not.toBe("sticky");
  });

  test("arrow keys navigate between column headers", async ({ page }) => {
    // Focus the first column header
    const firstTh = page.locator(".pt-th").first();
    await firstTh.focus();

    // Press ArrowRight to move to next column
    await firstTh.press("ArrowRight");
    const secondTh = page.locator(".pt-th").nth(1);
    await expect(secondTh).toBeFocused();

    // Press ArrowRight again
    await secondTh.press("ArrowRight");
    const thirdTh = page.locator(".pt-th").nth(2);
    await expect(thirdTh).toBeFocused();

    // Press ArrowLeft to go back
    await thirdTh.press("ArrowLeft");
    await expect(secondTh).toBeFocused();
  });

  test("Enter key triggers sort on focused column", async ({ page }) => {
    const scoreTh = page.locator(".pt-th").filter({ hasText: "Score" });
    await scoreTh.focus();
    await scoreTh.press("Enter");

    // Sort arrow should appear
    await expect(scoreTh.locator(".pt-sort-arrow")).toContainText("↑", {
      timeout: 2000,
    });

    // Press Enter again for descending
    await scoreTh.press("Enter");
    await expect(scoreTh.locator(".pt-sort-arrow")).toContainText("↓", {
      timeout: 2000,
    });
  });
});
