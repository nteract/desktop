import { test } from '@argo-video/cli';
import { showOverlay, showConfetti } from '@argo-video/cli';
import { spotlight, focusRing, zoomTo, resetCamera } from '@argo-video/cli';
import { cursorHighlight, trackCursor } from '@argo-video/cli';

/** Mark scene + let sift's render settle before overlay injection. */
async function mark(page: any, narration: any, scene: string) {
  narration.mark(scene);
  await page.waitForSelector('.pt-row', { state: 'visible', timeout: 5000 }).catch(() => {});
  await page.waitForTimeout(1000);
}

/** Scroll the table container by a given offset. */
async function scroll(page: any, container: any, top: number, smooth = false) {
  await container.evaluate((el: HTMLElement, opts: any) =>
    el.scrollBy({ top: opts.top, behavior: opts.smooth ? 'smooth' : undefined }),
    { top, smooth },
  );
}

/** Drag the mouse horizontally through a range of x-offsets (7px per step). */
async function dragX(page: any, sx: number, sy: number, from: number, to: number, delayMs: number) {
  const step = from < to ? 1 : -1;
  for (let i = from; step > 0 ? i <= to : i >= to; i += step) {
    await page.mouse.move(sx + i * 7, sy);
    await page.waitForTimeout(delayMs);
  }
}

test('sift-showcase', async ({ page, narration }) => {
  test.setTimeout(180000);

  await page.goto('/?dataset=spotify');
  trackCursor(page, narration);
  cursorHighlight(page, { color: '#60a5fa', radius: 18 });

  // Wait for all ~114k rows to stream in (odometer uses data-value attr)
  await page.waitForFunction(
    () => document.querySelector('.pt-stat-rows')?.getAttribute('data-value')?.includes('rows'),
    { timeout: 60000 },
  );
  // Extra settle time — HuggingFace data streams in row groups
  await page.waitForTimeout(2000);

  const headerRow = page.locator('.pt-header-row');
  const tableContainer = page.locator('.pt-table-container');
  // Spotify column order (from HF parquet schema):
  // 0:Unnamed:0 1:track_id 2:artists 3:album_name 4:track_name
  // 5:popularity 6:duration_ms 7:explicit 8:danceability 9:energy
  // 10:key ... 20:track_genre
  const col = (n: number) => headerRow.locator('.pt-th').nth(n);

  await mark(page, narration, 'intro');
  await showOverlay(page, 'intro', narration.durationFor('intro'));

  await mark(page, narration, 'summaries');
  const summaryDur = narration.durationFor('summaries');
  showOverlay(page, 'summaries', summaryDur);
  const beat = Math.floor(summaryDur / 3);
  const summaryColumns = [
    { col: col(5), color: '#60a5fa' },   // popularity (numeric histogram)
    { col: col(20), color: '#e879f9' },   // track_genre (categorical bars)
    { col: col(7), color: '#22d3ee' },    // explicit (boolean ratio bar)
  ];
  for (const { col: header, color } of summaryColumns) {
    await header.scrollIntoViewIfNeeded();
    focusRing(page, header, { color, duration: beat });
    await header.hover();
    await page.waitForTimeout(beat);
  }

  await mark(page, narration, 'scroll-fast');
  showOverlay(page, 'scroll-fast', narration.durationFor('scroll-fast'));
  focusRing(page, page.locator('.pt-stats'), { color: '#60a5fa', duration: 4000 });
  await scroll(page, tableContainer, 8000);
  await page.waitForTimeout(800);
  await scroll(page, tableContainer, 12000);
  await page.waitForTimeout(800);
  await scroll(page, tableContainer, -15000, true);
  await page.waitForTimeout(1800);
  await scroll(page, tableContainer, 20000);
  await page.waitForTimeout(600);
  await scroll(page, tableContainer, -25000, true);
  await page.waitForTimeout(1800);
  await tableContainer.evaluate((el: HTMLElement) => el.scrollTo({ top: 0 }));
  await page.waitForTimeout(500);

  await mark(page, narration, 'resize-text');
  const resizeDur = narration.durationFor('resize-text');
  showOverlay(page, 'resize-text', resizeDur);
  const resizeCol = col(4); // track_name — long titles that wrap well
  await resizeCol.scrollIntoViewIfNeeded();
  zoomTo(page, tableContainer, {
    narration, scale: 1.4, fadeIn: 800, fadeOut: 800,
    duration: resizeDur, holdMs: Math.floor(resizeDur * 0.7),
  });
  await page.waitForTimeout(800);
  const resizeHandle = resizeCol.locator('.pt-resize-handle');
  await resizeHandle.waitFor({ state: 'attached', timeout: 5000 });
  // Hover the handle to ensure Playwright targets it precisely (6px wide)
  await resizeHandle.hover({ force: true });
  await page.waitForTimeout(200);
  const handleBox = await resizeHandle.boundingBox();
  if (handleBox) {
    const sx = handleBox.x + handleBox.width / 2;
    const sy = handleBox.y + handleBox.height / 2;
    await page.mouse.move(sx, sy);
    await page.waitForTimeout(300);
    await page.mouse.down();
    await dragX(page, sx, sy, 0, 50, 40);     // expand
    await page.waitForTimeout(1200);
    await dragX(page, sx, sy, 50, -30, 35);   // contract past original
    await page.waitForTimeout(1200);
    await dragX(page, sx, sy, -30, 10, 30);   // settle
    await page.mouse.up();
  }
  await page.waitForTimeout(600);
  await resetCamera(page);

  // Sort by danceability — recognizable music metric
  const danceHeader = col(8);
  await danceHeader.scrollIntoViewIfNeeded();
  await danceHeader.locator('.pt-th-top').click();
  await mark(page, narration, 'sort');
  const sortDur = narration.durationFor('sort');
  showOverlay(page, 'sort', sortDur);
  spotlight(page, danceHeader, { duration: 2000, padding: 8 });
  await page.waitForTimeout(sortDur);

  // Brush filter on danceability histogram
  await mark(page, narration, 'brush-filter');
  const brushDur = narration.durationFor('brush-filter');
  showOverlay(page, 'brush-filter', brushDur);
  const box = await danceHeader.locator('.pt-th-summary').boundingBox();
  if (box) {
    const y = box.y + box.height / 2;
    const x0 = box.x + box.width * 0.2;
    const x1 = box.x + box.width * 0.75;
    await page.waitForTimeout(500);
    await page.mouse.move(x0, y);
    await page.waitForTimeout(200);
    await page.mouse.down();
    for (let s = 0; s <= 30; s++) {
      await page.mouse.move(x0 + (x1 - x0) * (s / 30), y);
      await page.waitForTimeout(40);
    }
    await page.mouse.up();
    await page.waitForTimeout(3000);
  }

  // Boolean filter — click "Yes" on the explicit column
  await mark(page, narration, 'boolean-filter');
  showOverlay(page, 'boolean-filter', narration.durationFor('boolean-filter'));
  const explicitHeader = col(7);
  await explicitHeader.scrollIntoViewIfNeeded();
  focusRing(page, explicitHeader, { color: '#22d3ee', duration: 2500 });
  await page.waitForTimeout(600);
  await explicitHeader.locator('.pt-bool-true').click();
  await page.waitForTimeout(3000);

  await mark(page, narration, 'clear');
  const clearDur = narration.durationFor('clear');
  showOverlay(page, 'clear', clearDur);
  focusRing(page, page.locator('.pt-filter-pills'), { color: '#e879f9', duration: 2000 });
  await page.waitForTimeout(600);
  const pills = page.locator('.pt-filter-pill-x');
  while ((await pills.count()) > 0) {
    await pills.first().click();
    await page.waitForTimeout(400);
  }
  await page.waitForTimeout(Math.max(500, clearDur - 3000));

  await mark(page, narration, 'darkmode');
  const darkDur = narration.durationFor('darkmode', { minMs: 3000 });
  showOverlay(page, 'darkmode', darkDur);
  const toggle = page.locator('#theme-toggle');
  focusRing(page, toggle, { color: '#f59e0b', duration: 1500 });
  await page.waitForTimeout(400);
  await toggle.click();
  await page.waitForTimeout(Math.max(500, darkDur - 1500));

  // Skip mark() settle for closing — go straight to the finale
  narration.mark('closing');
  showConfetti(page, { emoji: ['📊', '⚡', '🔥'], spread: 'burst', duration: 3000, pieces: 180 });
  showOverlay(page, 'closing', 4000);
  const starBtn = page.locator('.pt-github-btn');
  await page.waitForTimeout(1200);
  focusRing(page, starBtn, { color: '#f59e0b', duration: 2500 });
  await page.waitForTimeout(2800);
});
