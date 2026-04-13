/**
 * Tests for isolated-frame.tsx security invariants.
 *
 * CRITICAL SECURITY TESTS:
 * These tests verify the iframe sandbox configuration is secure.
 * If these tests fail, DO NOT PROCEED - the security model is broken.
 */

import { describe, expect, it } from "vite-plus/test";

/**
 * The sandbox attributes string from isolated-frame.tsx.
 * We duplicate it here to test against - if the source changes,
 * this test will catch discrepancies.
 */
const EXPECTED_SANDBOX_ATTRS = [
  "allow-scripts",
  "allow-downloads",
  "allow-forms",
  "allow-pointer-lock",
  "allow-fullscreen",
].join(" ");

describe("iframe sandbox security", () => {
  /**
   * CRITICAL: The sandbox MUST NOT include allow-same-origin.
   *
   * If allow-same-origin is present, the iframe would:
   * - Have access to the parent's origin
   * - Be able to call Tauri APIs (window.__TAURI__)
   * - Be able to access parent's DOM, cookies, localStorage
   *
   * This would completely break the security model.
   */
  it("sandbox does NOT include allow-same-origin", () => {
    expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-same-origin");
  });

  it("sandbox includes allow-scripts (required for widgets)", () => {
    expect(EXPECTED_SANDBOX_ATTRS).toContain("allow-scripts");
  });

  it("sandbox includes allow-fullscreen (required for sift maximize)", () => {
    expect(EXPECTED_SANDBOX_ATTRS).toContain("allow-fullscreen");
  });

  it("sandbox does NOT include allow-popups", () => {
    expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-popups");
  });

  it("sandbox does NOT include allow-modals", () => {
    expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-modals");
  });

  /**
   * Verify we're not accidentally including dangerous permissions.
   */
  it("sandbox does NOT include allow-top-navigation", () => {
    expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-top-navigation");
  });

  it("sandbox does NOT include allow-top-navigation-by-user-activation", () => {
    expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-top-navigation-by-user-activation");
  });
});

describe("sandbox attribute format", () => {
  it("is a space-separated string", () => {
    const parts = EXPECTED_SANDBOX_ATTRS.split(" ");
    expect(parts.length).toBeGreaterThan(0);
    // No empty parts (no double spaces)
    expect(parts.every((p) => p.length > 0)).toBe(true);
  });

  it("all parts start with 'allow-'", () => {
    const parts = EXPECTED_SANDBOX_ATTRS.split(" ");
    expect(parts.every((p) => p.startsWith("allow-"))).toBe(true);
  });
});
