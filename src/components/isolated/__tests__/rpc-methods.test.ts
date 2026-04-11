/**
 * Tests for rpc-methods.ts — JSON-RPC method constants and types.
 *
 * Verifies:
 * 1. Method constants have correct "nteract/" namespace prefix
 * 2. All method constants are unique
 * 3. Request vs notification method categorization
 */

import { describe, expect, it } from "vite-plus/test";
import {
  NTERACT_BRIDGE_READY,
  NTERACT_CLEAR_OUTPUTS,
  NTERACT_COMM_CLOSE,
  NTERACT_COMM_MSG,
  NTERACT_COMM_OPEN,
  NTERACT_WIDGET_SNAPSHOT,
  NTERACT_DOUBLE_CLICK,
  NTERACT_ERROR,
  NTERACT_EVAL,
  NTERACT_EVAL_RESULT,
  NTERACT_LINK_CLICK,
  NTERACT_PING,
  NTERACT_PONG,
  NTERACT_READY,
  NTERACT_RENDER_COMPLETE,
  NTERACT_RENDER_OUTPUT,
  NTERACT_RENDERER_READY,
  NTERACT_RESIZE,
  NTERACT_SEARCH,
  NTERACT_SEARCH_NAVIGATE,
  NTERACT_SEARCH_RESULTS,
  NTERACT_THEME,
  NTERACT_WIDGET_COMM_CLOSE,
  NTERACT_WIDGET_COMM_MSG,
  NTERACT_WIDGET_READY,
  NTERACT_WIDGET_STATE,
  NTERACT_WIDGET_UPDATE,
} from "../rpc-methods";

describe("nteract JSON-RPC method constants", () => {
  const ALL_METHODS = [
    NTERACT_EVAL,
    NTERACT_SEARCH,
    NTERACT_RENDER_OUTPUT,
    NTERACT_CLEAR_OUTPUTS,
    NTERACT_SEARCH_NAVIGATE,
    NTERACT_COMM_OPEN,
    NTERACT_COMM_MSG,
    NTERACT_COMM_CLOSE,
    NTERACT_WIDGET_SNAPSHOT,
    NTERACT_BRIDGE_READY,
    NTERACT_WIDGET_STATE,
    NTERACT_THEME,
    NTERACT_PING,
    NTERACT_READY,
    NTERACT_RENDERER_READY,
    NTERACT_RENDER_COMPLETE,
    NTERACT_RESIZE,
    NTERACT_LINK_CLICK,
    NTERACT_DOUBLE_CLICK,
    NTERACT_ERROR,
    NTERACT_WIDGET_READY,
    NTERACT_WIDGET_COMM_MSG,
    NTERACT_WIDGET_COMM_CLOSE,
    NTERACT_WIDGET_UPDATE,
    NTERACT_EVAL_RESULT,
    NTERACT_PONG,
    NTERACT_SEARCH_RESULTS,
  ];

  it("all methods have nteract/ namespace prefix", () => {
    for (const method of ALL_METHODS) {
      expect(method).toMatch(/^nteract\//);
    }
  });

  it("all method constants are unique", () => {
    const unique = new Set(ALL_METHODS);
    expect(unique.size).toBe(ALL_METHODS.length);
  });

  it("request methods return expected names", () => {
    expect(NTERACT_EVAL).toBe("nteract/eval");
    expect(NTERACT_SEARCH).toBe("nteract/search");
  });

  it("notification methods return expected names", () => {
    expect(NTERACT_RENDER_OUTPUT).toBe("nteract/renderOutput");
    expect(NTERACT_CLEAR_OUTPUTS).toBe("nteract/clearOutputs");
    expect(NTERACT_COMM_OPEN).toBe("nteract/commOpen");
    expect(NTERACT_COMM_MSG).toBe("nteract/commMsg");
    expect(NTERACT_COMM_CLOSE).toBe("nteract/commClose");
    expect(NTERACT_WIDGET_SNAPSHOT).toBe("nteract/widgetSnapshot");
    expect(NTERACT_BRIDGE_READY).toBe("nteract/bridgeReady");
    expect(NTERACT_WIDGET_READY).toBe("nteract/widgetReady");
    expect(NTERACT_WIDGET_COMM_MSG).toBe("nteract/widgetCommMsg");
    expect(NTERACT_WIDGET_COMM_CLOSE).toBe("nteract/widgetCommClose");
    expect(NTERACT_RENDER_COMPLETE).toBe("nteract/renderComplete");
    expect(NTERACT_DOUBLE_CLICK).toBe("nteract/doubleClick");
    expect(NTERACT_WIDGET_UPDATE).toBe("nteract/widgetUpdate");
    expect(NTERACT_WIDGET_STATE).toBe("nteract/widgetState");
    expect(NTERACT_SEARCH_NAVIGATE).toBe("nteract/searchNavigate");
  });
});
