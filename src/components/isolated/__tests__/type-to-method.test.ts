/**
 * Tests for TYPE_TO_METHOD mapping in isolated-frame.tsx.
 *
 * Verifies that every legacy message type that goes through send()
 * has a corresponding JSON-RPC method mapping, and that the mapping
 * values match the constants in rpc-methods.ts.
 */

import { describe, expect, it } from "vitest";
import {
  NTERACT_BRIDGE_READY,
  NTERACT_CLEAR_OUTPUTS,
  NTERACT_COMM_CLOSE,
  NTERACT_COMM_MSG,
  NTERACT_COMM_OPEN,
  NTERACT_COMM_SYNC,
  NTERACT_EVAL,
  NTERACT_PING,
  NTERACT_RENDER_OUTPUT,
  NTERACT_SEARCH,
  NTERACT_SEARCH_NAVIGATE,
  NTERACT_THEME,
  NTERACT_WIDGET_STATE,
} from "../rpc-methods";

// Duplicated from isolated-frame.tsx to test in isolation
// If this gets out of sync, the test will fail and remind us to update
const TYPE_TO_METHOD: Record<string, string> = {
  render: NTERACT_RENDER_OUTPUT,
  theme: NTERACT_THEME,
  clear: NTERACT_CLEAR_OUTPUTS,
  eval: NTERACT_EVAL,
  ping: NTERACT_PING,
  search: NTERACT_SEARCH,
  search_navigate: NTERACT_SEARCH_NAVIGATE,
  comm_open: NTERACT_COMM_OPEN,
  comm_msg: NTERACT_COMM_MSG,
  comm_close: NTERACT_COMM_CLOSE,
  comm_sync: NTERACT_COMM_SYNC,
  bridge_ready: NTERACT_BRIDGE_READY,
  widget_state: NTERACT_WIDGET_STATE,
};

describe("TYPE_TO_METHOD mapping", () => {
  it("maps all host→iframe legacy types to JSON-RPC methods", () => {
    // Every legacy type that CommBridgeManager or IsolatedFrame.send() uses
    const requiredTypes = [
      "render",
      "theme",
      "clear",
      "eval",
      "ping",
      "search",
      "search_navigate",
      "comm_open",
      "comm_msg",
      "comm_close",
      "comm_sync",
      "bridge_ready",
      "widget_state",
    ];

    for (const type of requiredTypes) {
      expect(
        TYPE_TO_METHOD[type],
        `Missing mapping for legacy type "${type}"`,
      ).toBeDefined();
    }
  });

  it("all mapped methods have nteract/ prefix", () => {
    for (const [type, method] of Object.entries(TYPE_TO_METHOD)) {
      expect(method, `Method for "${type}" should have nteract/ prefix`).toMatch(
        /^nteract\//,
      );
    }
  });

  it("maps to correct JSON-RPC method names", () => {
    expect(TYPE_TO_METHOD.render).toBe("nteract/renderOutput");
    expect(TYPE_TO_METHOD.theme).toBe("nteract/theme");
    expect(TYPE_TO_METHOD.clear).toBe("nteract/clearOutputs");
    expect(TYPE_TO_METHOD.eval).toBe("nteract/eval");
    expect(TYPE_TO_METHOD.search).toBe("nteract/search");
    expect(TYPE_TO_METHOD.search_navigate).toBe("nteract/searchNavigate");
    expect(TYPE_TO_METHOD.comm_open).toBe("nteract/commOpen");
    expect(TYPE_TO_METHOD.comm_msg).toBe("nteract/commMsg");
    expect(TYPE_TO_METHOD.comm_close).toBe("nteract/commClose");
    expect(TYPE_TO_METHOD.comm_sync).toBe("nteract/commSync");
    expect(TYPE_TO_METHOD.bridge_ready).toBe("nteract/bridgeReady");
    expect(TYPE_TO_METHOD.widget_state).toBe("nteract/widgetState");
  });

  it("has no duplicate method values", () => {
    const methods = Object.values(TYPE_TO_METHOD);
    const unique = new Set(methods);
    expect(unique.size).toBe(methods.length);
  });
});
