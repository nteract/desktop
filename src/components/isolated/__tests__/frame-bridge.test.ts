/**
 * Tests for frame-bridge.ts message protocol types and guards.
 *
 * These tests verify:
 * 1. Message type guards work correctly
 * 2. The message type whitelist is complete
 */

import { describe, expect, it } from "vite-plus/test";
import { type IframeToParentMessage, isIframeMessage, isMessageType } from "../frame-bridge";

describe("isIframeMessage", () => {
  // Valid message types that should pass
  const validMessageTypes = [
    "ready",
    "pong",
    "eval_result",
    "render_complete",
    "resize",
    "link_click",
    "widget_update",
    "error",
    "renderer_ready",
    "widget_ready",
    "widget_comm_msg",
    "widget_comm_close",
  ] as const;

  it.each(validMessageTypes)('returns true for valid message type "%s"', (type) => {
    expect(isIframeMessage({ type })).toBe(true);
  });

  it("returns false for null", () => {
    expect(isIframeMessage(null)).toBe(false);
  });

  it("returns false for undefined", () => {
    expect(isIframeMessage(undefined)).toBe(false);
  });

  it("returns false for primitives", () => {
    expect(isIframeMessage("ready")).toBe(false);
    expect(isIframeMessage(123)).toBe(false);
    expect(isIframeMessage(true)).toBe(false);
  });

  it("returns false for objects without type", () => {
    expect(isIframeMessage({})).toBe(false);
    expect(isIframeMessage({ payload: {} })).toBe(false);
  });

  it("returns false for objects with non-string type", () => {
    expect(isIframeMessage({ type: 123 })).toBe(false);
    expect(isIframeMessage({ type: null })).toBe(false);
    expect(isIframeMessage({ type: {} })).toBe(false);
  });

  it("returns false for unknown message types", () => {
    expect(isIframeMessage({ type: "unknown" })).toBe(false);
    expect(isIframeMessage({ type: "READY" })).toBe(false);
    expect(isIframeMessage({ type: "Ready" })).toBe(false);
  });

  // Parent-to-iframe message types should NOT pass
  const parentMessageTypes = [
    "render",
    "theme",
    "clear",
    "ping",
    "eval",
    "bridge_ready",
    "comm_open",
    "comm_msg",
    "comm_close",
    "widget_snapshot",
  ];

  it.each(parentMessageTypes)('returns false for parent message type "%s"', (type) => {
    expect(isIframeMessage({ type })).toBe(false);
  });
});

describe("isMessageType", () => {
  it("returns true when type matches", () => {
    const msg = { type: "ready" };
    expect(isMessageType(msg, "ready")).toBe(true);
  });

  it("returns false when type does not match", () => {
    const msg = { type: "ready" };
    expect(isMessageType(msg, "error")).toBe(false);
  });

  it("returns false for null", () => {
    expect(isMessageType(null, "ready")).toBe(false);
  });

  it("returns false for non-objects", () => {
    expect(isMessageType("ready", "ready")).toBe(false);
  });
});

describe("message type whitelist completeness", () => {
  /**
   * CRITICAL: This test ensures the whitelist in isIframeMessage stays in sync
   * with the IframeToParentMessage type. If you add a new message type to
   * IframeToParentMessage, you MUST add it to the whitelist in isIframeMessage.
   */
  it("whitelist matches all IframeToParentMessage types", () => {
    // These are all the types in IframeToParentMessage union
    // If this test fails, the whitelist in isIframeMessage needs updating
    const allIframeMessageTypes: IframeToParentMessage["type"][] = [
      "ready",
      "pong",
      "eval_result",
      "render_complete",
      "resize",
      "link_click",
      "widget_update",
      "error",
      "renderer_ready",
      "widget_ready",
      "widget_comm_msg",
      "widget_comm_close",
    ];

    for (const type of allIframeMessageTypes) {
      expect(isIframeMessage({ type }), `Message type "${type}" should be in whitelist`).toBe(true);
    }
  });
});
