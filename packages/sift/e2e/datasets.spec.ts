import { expect, test } from "@playwright/test";

test.describe("Dataset Picker", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".pt-table-container");
  });

  test("shows dataset dropdown with all options", async ({ page }) => {
    const select = page.locator("#dataset-select");
    await expect(select).toBeVisible();

    const options = select.locator("option");
    await expect(options).toHaveCount(8);
    await expect(options.first()).toContainText("Generated");
  });

  test("can load generated dataset via URL param", async ({ page }) => {
    const select = page.locator("#dataset-select");
    await expect(select).toHaveValue("generated");

    const description = page.locator("#dataset-description");
    await expect(description).toContainText("synthetic");
  });

  test("default dataset without param selects Spotify in picker", async ({
    page,
  }) => {
    await page.goto("/");
    // Just check the select value — don't wait for the table to load (requires network)
    const select = page.locator("#dataset-select");
    await expect(select).toHaveValue("spotify");
  });
});
