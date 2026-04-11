import { expect, test } from "@playwright/test";

/**
 * Performance benchmarks for the pretext-table engine.
 *
 * These tests measure real-world performance in a headless browser.
 * Results are logged as structured JSON for tracking over time.
 * Tests always pass — the numbers are the signal, not assertions.
 *
 * Run with: npx playwright test e2e/perf.spec.ts
 */

type PerfResult = { metric: string; value: number; unit: string };

const results: PerfResult[] = [];

function record(metric: string, value: number, unit: string) {
  results.push({ metric, value, unit });
  const formatted =
    unit === "µs"
      ? `${value.toFixed(0)}µs`
      : unit === "ms"
        ? `${value.toFixed(1)}ms`
        : `${value}${unit}`;
  console.log(`  ⏱  ${metric.padEnd(40)} ${formatted}`);
}

test.describe("Performance Benchmarks (100k rows)", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".pt-table-container");
    // Wait for all 100k rows to stream in
    await expect(page.locator(".pt-stat-rows")).toHaveAttribute("data-value", /100,000/, {
      timeout: 30_000,
    });
  });

  test("mount and stream all batches", async ({ page }) => {
    const start = Date.now();
    await page.goto("/?dataset=generated");
    await page.waitForSelector(".pt-table-container");
    const firstBatch = Date.now() - start;

    await expect(page.locator(".pt-stat-rows")).toHaveAttribute("data-value", /100,000/, {
      timeout: 30_000,
    });
    const allBatches = Date.now() - start;

    console.log("\n📊 Mount & Stream (100k rows, 12 columns):");
    record("first_batch_visible", firstBatch, "ms");
    record("all_batches_streamed", allBatches, "ms");
  });

  test("scroll frame time", async ({ page }) => {
    const viewport = page.locator(".pt-viewport");

    await viewport.evaluate((el) => (el.scrollTop = 1000));
    await page.waitForTimeout(100);

    const times = await viewport.evaluate((el) => {
      return new Promise<number[]>((resolve) => {
        const results: number[] = [];
        let step = 0;
        function tick() {
          const t0 = performance.now();
          el.scrollTop = 2000 + step * 500;
          requestAnimationFrame(() => {
            results.push(performance.now() - t0);
            step++;
            if (step < 20) tick();
            else resolve(results);
          });
        }
        tick();
      });
    });

    const avg = times.reduce((a, b) => a + b) / times.length;
    const max = Math.max(...times);
    const p95 = times.sort((a, b) => a - b)[Math.floor(times.length * 0.95)];

    console.log("\n📊 Scroll (20 frames across 10k rows):");
    record("scroll_avg_frame", avg, "ms");
    record("scroll_p95_frame", p95, "ms");
    record("scroll_max_frame", max, "ms");
  });

  test("sort response time", async ({ page }) => {
    const scoreTh = page.locator(".pt-th").filter({ hasText: "Score" });

    const sortTime = await page.evaluate(() => {
      return new Promise<number>((resolve) => {
        // Click .pt-th-top (sort handler) not .pt-th (outer container)
        const top = document.querySelector(".pt-th:nth-child(8) .pt-th-top") as HTMLElement;
        const t0 = performance.now();
        top.click();
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            resolve(performance.now() - t0);
          });
        });
      });
    });

    console.log("\n📊 Sort (100k rows):");
    record("sort_click_to_render", sortTime, "ms");

    // Verify sort actually applied
    await expect(scoreTh.locator(".pt-sort-arrow")).toContainText("↑");
  });

  test("filter response time", async ({ page }) => {
    const scoreTh = page.locator(".pt-th", { hasText: "SCORE" });
    const summary = scoreTh.locator(".pt-th-summary");
    const box = await summary.boundingBox();
    if (!box) throw new Error("No summary bounding box");

    const filterTime = await page.evaluate(
      ({ x, y, w, h }) => {
        return new Promise<number>((resolve) => {
          const svg = document.querySelector(
            ".pt-th:nth-child(8) .pt-th-summary svg:last-child",
          ) as SVGElement;
          if (!svg) {
            resolve(-1);
            return;
          }

          const t0 = performance.now();
          svg.dispatchEvent(
            new PointerEvent("pointerdown", {
              clientX: x + 10,
              clientY: y + h / 2,
              bubbles: true,
            }),
          );
          svg.dispatchEvent(
            new PointerEvent("pointermove", {
              clientX: x + w / 2,
              clientY: y + h / 2,
              bubbles: true,
            }),
          );
          svg.dispatchEvent(
            new PointerEvent("pointerup", {
              clientX: x + w / 2,
              clientY: y + h / 2,
              bubbles: true,
            }),
          );

          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              resolve(performance.now() - t0);
            });
          });
        });
      },
      { x: box.x, y: box.y, w: box.width, h: box.height },
    );

    console.log("\n📊 Filter (100k rows):");
    record("filter_brush_to_render", filterTime, "ms");
  });

  test("column resize frame time", async ({ page }) => {
    const nameTh = page.locator(".pt-th").nth(1);
    const handle = nameTh.locator(".pt-resize-handle");
    const handleBox = await handle.boundingBox();
    if (!handleBox) throw new Error("No handle bounding box");

    const resizeTimes = await page.evaluate(
      ({ x, y }) => {
        return new Promise<number[]>((resolve) => {
          const handle = document.querySelector(
            ".pt-th:nth-child(2) .pt-resize-handle",
          ) as HTMLElement;
          if (!handle) {
            resolve([]);
            return;
          }

          handle.dispatchEvent(
            new PointerEvent("pointerdown", {
              clientX: x,
              clientY: y,
              pointerId: 1,
              bubbles: true,
            }),
          );

          const results: number[] = [];
          let step = 0;
          function tick() {
            const t0 = performance.now();
            handle.dispatchEvent(
              new PointerEvent("pointermove", {
                clientX: x + step * 5,
                clientY: y,
                pointerId: 1,
                bubbles: true,
              }),
            );
            document.body.offsetHeight;
            results.push(performance.now() - t0);
            step++;
            if (step < 20) requestAnimationFrame(tick);
            else {
              handle.dispatchEvent(
                new PointerEvent("pointerup", {
                  clientX: x + 100,
                  clientY: y,
                  pointerId: 1,
                  bubbles: true,
                }),
              );
              resolve(results);
            }
          }
          requestAnimationFrame(tick);
        });
      },
      {
        x: handleBox.x + handleBox.width / 2,
        y: handleBox.y + handleBox.height / 2,
      },
    );

    if (resizeTimes.length > 0) {
      const avg = resizeTimes.reduce((a, b) => a + b) / resizeTimes.length;
      const max = Math.max(...resizeTimes);

      console.log("\n📊 Column Resize (20 drag frames, 100k rows):");
      record("resize_avg_frame", avg * 1000, "µs");
      record("resize_max_frame", max * 1000, "µs");
    }

    // Log all results as JSON for programmatic consumption
    console.log("\n📋 Results JSON:");
    console.log(JSON.stringify(results, null, 2));
  });
});
