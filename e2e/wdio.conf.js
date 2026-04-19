/**
 * WebDriverIO configuration for Tauri E2E testing
 *
 * Uses tauri-plugin-webdriver which embeds a W3C WebDriver server inside the
 * app on port 4445 by default. No external tauri-driver process is needed.
 *
 *   - Build: cargo build --features e2e-webdriver -p notebook
 *   - Run:   ./target/debug/notebook (WebDriver server starts automatically)
 *   - Test:  pnpm test:e2e:native
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Screenshot directory: configurable via env, defaults to ./e2e-screenshots
const SCREENSHOT_DIR =
  process.env.E2E_SCREENSHOT_DIR ||
  path.join(__dirname, "..", "e2e-screenshots");
const SCREENSHOT_FAILURES_DIR = path.join(SCREENSHOT_DIR, "failures");

// Ensure screenshot directories exist
fs.mkdirSync(SCREENSHOT_FAILURES_DIR, { recursive: true });

// Fixture specs require special setup (NOTEBOOK_PATH or working directory) and are excluded
// from the default run. Use ./e2e/dev.sh test-fixture or test-untitled-pyproject to run them.
const FIXTURE_SPECS = [
  "cell-visibility.spec.js", // Requires fixture notebook with pre-existing outputs
  "conda-inline.spec.js",
  "deno.spec.js",
  "prewarmed-uv.spec.js",
  "run-all-output-lifecycle.spec.js", // Requires fixture with stale outputs
  "trust-dialog-dismiss.spec.js",
  "untitled-pyproject.spec.js", // Requires working dir to be pyproject fixture directory
  "uv-inline.spec.js",
  "uv-pyproject.spec.js",
  "widget-slider-stall.spec.js", // Requires fixture notebook with ipywidgets slider
];

/**
 * Create settings file to skip onboarding screen in E2E tests.
 * Settings path varies by platform:
 * - Linux: ~/.config/nteract/settings.json
 * - macOS: ~/Library/Application Support/nteract/settings.json
 */
function ensureOnboardingSkipped() {
  const homeDir = process.env.HOME || process.env.USERPROFILE;
  const settingsDir =
    process.platform === "darwin"
      ? path.join(homeDir, "Library", "Application Support", "nteract")
      : path.join(homeDir, ".config", "nteract");
  const settingsPath = path.join(settingsDir, "settings.json");

  fs.mkdirSync(settingsDir, { recursive: true });

  // Read existing settings or start fresh
  let settings = {};
  try {
    settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
  } catch {
    // File doesn't exist or is invalid
  }

  // Mark onboarding as completed so tests don't get stuck
  settings.onboarding_completed = true;
  fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));
}

export const config = {
  runner: "local",

  specs: process.env.E2E_SPEC
    ? [path.resolve(process.env.E2E_SPEC)]
    : [path.join(__dirname, "specs", "*.spec.js")],

  // Exclude fixture specs from default run (they require NOTEBOOK_PATH)
  exclude: process.env.E2E_SPEC
    ? []
    : FIXTURE_SPECS.map((spec) => path.join(__dirname, "specs", spec)),

  // Don't run tests in parallel - we have one app instance
  maxInstances: 1,

  // Tauri WebDriver capabilities
  // tauri-plugin-webdriver embeds the server inside the app — no external driver needed
  capabilities: [
    {
      // Tauri uses wry as the browser engine
      browserName: "wry",
    },
  ],

  // WebDriver connection settings
  hostname: process.env.WEBDRIVER_HOST || "localhost",
  port: parseInt(
    process.env.WEBDRIVER_PORT ||
      process.env.CONDUCTOR_PORT ||
      process.env.PORT ||
      "4445",
    10,
  ),

  logLevel: "warn",

  // Timeouts
  waitforTimeout: 10000,
  connectionRetryTimeout: 120000,
  connectionRetryCount: 3,

  // Test framework
  framework: "mocha",
  reporters: ["spec"],

  // Retry failed spec files once — helps with transient timing issues
  // (IPC latency, pool warming, env creation). For trust-dependent tests,
  // waitForKernelReadyWithTrust handles trust inline so retries are safe.
  specFileRetries: 1,

  mochaOpts: {
    ui: "bdd",
    timeout: 780000, // 13 minutes — conda inline env creation can take 12 min on cold CI
  },

  /**
   * Hook that gets executed before any workers launch.
   * Creates settings file to skip onboarding screen.
   */
  onPrepare: () => {
    ensureOnboardingSkipped();
  },

  /**
   * Hook that gets executed after a test
   * Captures screenshot on failure for debugging
   */
  afterTest: async (test, context, { error }) => {
    if (error) {
      const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
      const safeName = test.title.replace(/[^a-zA-Z0-9]/g, "-").slice(0, 50);
      const screenshotPath = path.join(
        SCREENSHOT_FAILURES_DIR,
        `${safeName}-${timestamp}.png`,
      );
      try {
        const { browser } = await import("@wdio/globals");
        await browser.saveScreenshot(screenshotPath);
        console.log(`Failure screenshot saved: ${screenshotPath}`);
      } catch (screenshotError) {
        console.error("Failed to capture screenshot:", screenshotError.message);
      }
    }
  },
};
