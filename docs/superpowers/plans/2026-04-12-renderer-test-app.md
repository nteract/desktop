# Renderer Plugin Test App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone Vite app at `apps/renderer-test/` that exercises the full iframe renderer plugin pipeline (build, load, render) with Playwright verification, without Tauri or the daemon.

**Architecture:** Reuses the shared `src/` isolated renderer infrastructure — `IsolatedRendererProvider` with virtual module loader, `IsolatedFrame` for sandboxed iframes, `isolatedRendererPlugin()` Vite plugin to build the IIFE + all renderer plugins from source. A fixture array drives the page: each fixture is a MIME type + data pair rendered in its own iframe. Playwright tests verify each fixture renders without errors.

**Tech Stack:** React 19, Vite Plus, Playwright, TypeScript

**Spec:** `docs/superpowers/specs/2026-04-12-renderer-test-app-design.md`

---

## File Structure

```
apps/renderer-test/
  index.html
  package.json
  tsconfig.json
  vite.config.ts
  playwright.config.ts
  src/
    main.tsx
    fixtures.ts
    vite-env.d.ts
  e2e/
    render.spec.ts
```

---

### Task 1: Scaffold the app (package.json, tsconfig, index.html)

**Files:**
- Create: `apps/renderer-test/package.json`
- Create: `apps/renderer-test/tsconfig.json`
- Create: `apps/renderer-test/index.html`

- [ ] **Step 1: Create package.json**

Create `apps/renderer-test/package.json`:

```json
{
  "name": "renderer-test",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vp dev",
    "build": "vp build",
    "test:e2e": "playwright test"
  },
  "dependencies": {
    "react": "^19.1.0",
    "react-dom": "^19.1.0"
  },
  "devDependencies": {
    "@playwright/test": "^1.59.1",
    "@tailwindcss/vite": "^4.0.0",
    "@types/react": "^19.1.0",
    "@types/react-dom": "^19.1.0",
    "typescript": "^5.0.0",
    "vite": "catalog:",
    "vite-plus": "catalog:"
  }
}
```

- [ ] **Step 2: Create tsconfig.json**

Create `apps/renderer-test/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "dist",
    "rootDir": "src",
    "baseUrl": ".",
    "paths": {
      "@/*": ["../../src/*"]
    }
  },
  "include": ["src"]
}
```

- [ ] **Step 3: Create index.html**

Create `apps/renderer-test/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Renderer Plugin Test</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 4: Commit**

```bash
git add apps/renderer-test/package.json apps/renderer-test/tsconfig.json apps/renderer-test/index.html
git commit -m "feat(renderer-test): scaffold app with package.json, tsconfig, index.html"
```

---

### Task 2: Vite config with isolated renderer plugin

**Files:**
- Create: `apps/renderer-test/vite.config.ts`
- Create: `apps/renderer-test/src/vite-env.d.ts`

- [ ] **Step 1: Create vite.config.ts**

The config reuses the same `isolatedRendererPlugin()` Vite plugin from the notebook app. This builds the IIFE renderer bundle and all renderer plugins (markdown, plotly, vega, leaflet) from source, exposing them as virtual modules.

Create `apps/renderer-test/vite.config.ts`:

```typescript
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import path from "path";
import { defineConfig } from "vite-plus";
import { isolatedRendererPlugin } from "../notebook/vite-plugin-isolated-renderer";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    isolatedRendererPlugin({ minify: false }),
  ],
  resolve: {
    alias: {
      "@/": path.resolve(__dirname, "../../src") + "/",
    },
  },
  server: {
    port: 5176,
    strictPort: true,
  },
});
```

- [ ] **Step 2: Create vite-env.d.ts**

Create `apps/renderer-test/src/vite-env.d.ts`:

```typescript
/// <reference types="vite-plus/client" />

declare module "virtual:isolated-renderer" {
  export const rendererCode: string;
  export const rendererCss: string;
}

declare module "virtual:renderer-plugin/markdown" {
  export const code: string;
  export const css: string;
}

declare module "virtual:renderer-plugin/vega" {
  export const code: string;
  export const css: string;
}

declare module "virtual:renderer-plugin/plotly" {
  export const code: string;
  export const css: string;
}

declare module "virtual:renderer-plugin/leaflet" {
  export const code: string;
  export const css: string;
}
```

- [ ] **Step 3: Commit**

```bash
git add apps/renderer-test/vite.config.ts apps/renderer-test/src/vite-env.d.ts
git commit -m "feat(renderer-test): add Vite config with isolated renderer plugin"
```

---

### Task 3: Fixtures and main app

**Files:**
- Create: `apps/renderer-test/src/fixtures.ts`
- Create: `apps/renderer-test/src/main.tsx`

- [ ] **Step 1: Create fixtures.ts**

Each fixture defines a MIME type and data payload. These exercise both built-in renderers and plugin-loaded renderers.

Create `apps/renderer-test/src/fixtures.ts`:

```typescript
export interface Fixture {
  label: string;
  mimeType: string;
  data: unknown;
}

export const fixtures: Fixture[] = [
  {
    label: "Plain text",
    mimeType: "text/plain",
    data: "Hello from the renderer test app.\nThis is a second line.",
  },
  {
    label: "HTML",
    mimeType: "text/html",
    data: '<h2 style="color: steelblue;">HTML Output</h2><p>Rendered inside an isolated iframe.</p>',
  },
  {
    label: "JSON",
    mimeType: "application/json",
    data: JSON.stringify(
      { name: "renderer-test", version: "1.0.0", features: ["iframe", "plugins", "security"] },
      null,
      2,
    ),
  },
  {
    label: "SVG",
    mimeType: "image/svg+xml",
    data: '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"><rect width="200" height="100" rx="10" fill="#4f46e5"/><text x="100" y="55" text-anchor="middle" fill="white" font-family="system-ui" font-size="16">SVG Output</text></svg>',
  },
  {
    label: "Markdown (plugin)",
    mimeType: "text/markdown",
    data: "# Markdown Plugin\n\nThis is rendered by the **markdown renderer plugin**.\n\n- Item 1\n- Item 2\n- Item 3\n\n```python\nprint('hello')\n```\n",
  },
  {
    label: "Plotly (plugin)",
    mimeType: "application/vnd.plotly.v1+json",
    data: JSON.stringify({
      data: [
        {
          x: [1, 2, 3, 4, 5],
          y: [2, 6, 3, 8, 5],
          type: "scatter",
          mode: "lines+markers",
          name: "Test Series",
        },
      ],
      layout: {
        title: "Plotly Plugin Test",
        width: 500,
        height: 300,
      },
    }),
  },
];
```

- [ ] **Step 2: Create main.tsx**

The main app renders each fixture in its own `IsolatedFrame`, wrapped in an `IsolatedRendererProvider` that loads the renderer bundle via virtual module. Plugin injection happens automatically via `injectPluginsForMimes`.

Create `apps/renderer-test/src/main.tsx`:

```tsx
import { IsolatedFrame, type IsolatedFrameHandle } from "@/components/isolated/isolated-frame";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";
import { injectPluginsForMimes, needsPlugin } from "@/components/isolated/iframe-libraries";
import { createRoot } from "react-dom/client";
import { useCallback, useRef, useState } from "react";
import { fixtures, type Fixture } from "./fixtures";

function FixtureCard({ fixture, index }: { fixture: Fixture; index: number }) {
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const injectedRef = useRef(new Set<string>());
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onReady = useCallback(async () => {
    const frame = frameRef.current;
    if (!frame) return;

    // Install plugin if this MIME type needs one
    if (needsPlugin(fixture.mimeType)) {
      await injectPluginsForMimes(frame, [fixture.mimeType], injectedRef.current);
    }

    // Send the render message
    frame.render({
      mimeType: fixture.mimeType,
      data: fixture.data,
      cellId: `fixture-${index}`,
      outputIndex: 0,
    });

    setReady(true);
  }, [fixture, index]);

  return (
    <div
      style={{
        border: "1px solid #e0e0e0",
        borderRadius: 8,
        overflow: "hidden",
        marginBottom: 16,
      }}
    >
      <div
        style={{
          padding: "8px 12px",
          background: "#f5f5f5",
          borderBottom: "1px solid #e0e0e0",
          display: "flex",
          alignItems: "center",
          gap: 8,
          fontFamily: "system-ui, sans-serif",
          fontSize: 13,
        }}
      >
        <span
          data-testid={`fixture-status-${index}`}
          data-ready={ready}
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: error ? "#ef4444" : ready ? "#22c55e" : "#d4d4d4",
          }}
        />
        <strong>{fixture.label}</strong>
        <code style={{ color: "#6b7280", fontSize: 11 }}>{fixture.mimeType}</code>
        {error && <span style={{ color: "#ef4444", fontSize: 11 }}>{error}</span>}
      </div>
      <div data-testid={`fixture-frame-${index}`}>
        <IsolatedFrame
          ref={frameRef}
          id={`fixture-${index}`}
          onReady={onReady}
          onError={(e) => setError(e.message)}
          autoHeight
          minHeight={60}
        />
      </div>
    </div>
  );
}

function App() {
  return (
    <IsolatedRendererProvider loader={() => import("virtual:isolated-renderer")}>
      <div
        style={{
          maxWidth: 900,
          margin: "0 auto",
          padding: "24px 16px",
          fontFamily: "system-ui, sans-serif",
        }}
      >
        <h1 style={{ fontSize: 20, fontWeight: 700, marginBottom: 4 }}>
          Renderer Plugin Test
        </h1>
        <p style={{ color: "#6b7280", fontSize: 13, marginBottom: 24 }}>
          {fixtures.length} fixtures — each rendered in an isolated iframe
        </p>
        {fixtures.map((fixture, i) => (
          <FixtureCard key={i} fixture={fixture} index={i} />
        ))}
      </div>
    </IsolatedRendererProvider>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
```

- [ ] **Step 3: Install dependencies and verify dev server starts**

```bash
cd apps/renderer-test && pnpm install && npx vp dev &
sleep 5 && curl -s -o /dev/null -w "%{http_code}" http://localhost:5176
kill %1
```

Expected: HTTP 200 from the dev server.

If pnpm install fails, run `pnpm install` from the repo root instead (workspace-level).

- [ ] **Step 4: Commit**

```bash
git add apps/renderer-test/src/fixtures.ts apps/renderer-test/src/main.tsx
git commit -m "feat(renderer-test): add fixture definitions and main React app"
```

---

### Task 4: Playwright config and smoke test

**Files:**
- Create: `apps/renderer-test/playwright.config.ts`
- Create: `apps/renderer-test/e2e/render.spec.ts`

- [ ] **Step 1: Create playwright.config.ts**

Create `apps/renderer-test/playwright.config.ts`:

```typescript
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  timeout: 60_000,
  retries: process.env.CI ? 2 : 0,
  use: {
    baseURL: "http://localhost:5176",
    headless: true,
  },
  webServer: {
    command: "npx vp dev",
    port: 5176,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
});
```

- [ ] **Step 2: Create render.spec.ts**

The test navigates to the page, waits for each fixture's iframe to become ready, and verifies no errors.

Create `apps/renderer-test/e2e/render.spec.ts`:

```typescript
import { test, expect } from "@playwright/test";
import { fixtures } from "../src/fixtures";

test.describe("Renderer plugin fixtures", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("all fixtures render without errors", async ({ page }) => {
    // Wait for all fixture status indicators to appear
    for (let i = 0; i < fixtures.length; i++) {
      const status = page.locator(`[data-testid="fixture-status-${i}"]`);
      await expect(status).toBeVisible({ timeout: 30_000 });

      // Wait for the ready state (green dot) — polls until ready="true"
      await expect(status).toHaveAttribute("data-ready", "true", {
        timeout: 30_000,
      });
    }

    // Verify no error states
    for (let i = 0; i < fixtures.length; i++) {
      const status = page.locator(`[data-testid="fixture-status-${i}"]`);
      const readyAttr = await status.getAttribute("data-ready");
      expect(readyAttr).toBe("true");
    }
  });

  test("iframes have non-zero height", async ({ page }) => {
    // Wait for all to be ready first
    for (let i = 0; i < fixtures.length; i++) {
      const status = page.locator(`[data-testid="fixture-status-${i}"]`);
      await expect(status).toHaveAttribute("data-ready", "true", {
        timeout: 30_000,
      });
    }

    // Check each iframe has content
    const iframes = page.locator("iframe");
    const count = await iframes.count();
    expect(count).toBe(fixtures.length);

    for (let i = 0; i < count; i++) {
      const iframe = iframes.nth(i);
      const box = await iframe.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.height).toBeGreaterThan(10);
    }
  });
});
```

- [ ] **Step 3: Install Playwright browsers**

```bash
cd apps/renderer-test && npx playwright install chromium
```

- [ ] **Step 4: Run the tests**

```bash
cd apps/renderer-test && npx playwright test --reporter=line
```

Expected: both tests pass. If they fail, debug by running `npx vp dev` and opening `http://localhost:5176` in a browser to see which fixtures aren't rendering.

- [ ] **Step 5: Commit**

```bash
git add apps/renderer-test/playwright.config.ts apps/renderer-test/e2e/render.spec.ts
git commit -m "feat(renderer-test): add Playwright config and fixture smoke tests"
```

---

### Task 5: Lint, verify, clean up

**Files:** None (verification only)

- [ ] **Step 1: Run lint**

```bash
cargo xtask lint --fix 2>&1 | tail -15
```

- [ ] **Step 2: Verify the app builds**

```bash
cd apps/renderer-test && npx vp build 2>&1 | tail -10
```

Expected: production build succeeds.

- [ ] **Step 3: Run Playwright tests one more time**

```bash
cd apps/renderer-test && npx playwright test --reporter=line
```

Expected: all pass.

- [ ] **Step 4: Commit any lint fixes**

```bash
git add -A
git commit -m "style: lint fixes" || echo "nothing to commit"
```
