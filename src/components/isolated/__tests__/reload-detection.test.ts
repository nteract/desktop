/**
 * Tests for IsolatedFrame's reload detection state machine.
 *
 * The iframe sends "ready" on load. A second "ready" means the browser
 * tore down and reloaded the iframe (e.g. React moved the DOM node).
 * The component must detect this and re-bootstrap the renderer.
 *
 * This tests the pure state machine logic, not the React component.
 */

import { describe, expect, it } from "vite-plus/test";

// ── Extracted state machine from isolated-frame.tsx ─────────────

interface IframeState {
  hasReceivedReady: boolean;
  isReady: boolean;
  isContentRendered: boolean;
  isReloading: boolean;
  isIframeReady: boolean;
  bootstrapping: boolean;
  pendingMessages: unknown[];
}

function initialState(): IframeState {
  return {
    hasReceivedReady: false,
    isReady: false,
    isContentRendered: false,
    isReloading: false,
    isIframeReady: false,
    bootstrapping: false,
    pendingMessages: [],
  };
}

type ReadyResult = { kind: "initial_load" } | { kind: "reload" };

/**
 * Mirrors the "ready" message handler in isolated-frame.tsx.
 * Returns what kind of ready this was, and mutates state accordingly.
 */
function handleReady(state: IframeState): ReadyResult {
  const isReload = state.hasReceivedReady;
  state.hasReceivedReady = true;

  if (isReload) {
    // Reset bootstrap state so renderer gets re-injected
    state.bootstrapping = false;
    // Keep imperative readiness in sync
    state.isReady = false;
    // Drop pending messages targeted at old iframe instance
    state.pendingMessages.length = 0;
    state.isContentRendered = false;
    // Hide iframe during reload to prevent white flash
    state.isReloading = true;
    // Toggle isIframeReady to force effects to re-run
    state.isIframeReady = false;
    // setTimeout(() => state.isIframeReady = true, 0) happens async

    return { kind: "reload" };
  }

  state.isIframeReady = true;
  return { kind: "initial_load" };
}

/**
 * Mirrors the "renderer_ready" message handler.
 */
function handleRendererReady(state: IframeState): void {
  state.isReady = true;
  state.isReloading = false;
}

// ── Tests ──────────────────────────────────────────────────────────

describe("iframe reload detection state machine", () => {
  it("treats first ready as initial load", () => {
    const state = initialState();
    const result = handleReady(state);

    expect(result.kind).toBe("initial_load");
    expect(state.hasReceivedReady).toBe(true);
    expect(state.isIframeReady).toBe(true);
    expect(state.isReloading).toBe(false);
  });

  it("treats second ready as reload", () => {
    const state = initialState();
    handleReady(state); // first ready
    const result = handleReady(state); // second ready = reload

    expect(result.kind).toBe("reload");
    expect(state.isReloading).toBe(true);
    expect(state.isReady).toBe(false);
    expect(state.isContentRendered).toBe(false);
    expect(state.bootstrapping).toBe(false);
  });

  it("drops pending messages on reload", () => {
    const state = initialState();
    handleReady(state);
    state.pendingMessages.push({ type: "render", payload: "old" });
    state.pendingMessages.push({ type: "theme", dark: true });

    handleReady(state); // reload

    expect(state.pendingMessages).toHaveLength(0);
  });

  it("resets isIframeReady to false on reload (for effect re-trigger)", () => {
    const state = initialState();
    handleReady(state);
    expect(state.isIframeReady).toBe(true);

    handleReady(state); // reload
    expect(state.isIframeReady).toBe(false);
    // In the real component, setTimeout sets it back to true
  });

  it("renderer_ready after reload clears reloading state", () => {
    const state = initialState();
    handleReady(state); // initial
    handleRendererReady(state); // renderer initialized
    expect(state.isReady).toBe(true);

    handleReady(state); // reload — resets isReady
    expect(state.isReady).toBe(false);
    expect(state.isReloading).toBe(true);

    handleRendererReady(state); // renderer re-initialized
    expect(state.isReady).toBe(true);
    expect(state.isReloading).toBe(false);
  });

  it("handles multiple consecutive reloads", () => {
    const state = initialState();
    handleReady(state); // initial
    handleRendererReady(state);

    // Reload 1
    handleReady(state);
    expect(state.isReloading).toBe(true);
    handleRendererReady(state);
    expect(state.isReloading).toBe(false);

    // Reload 2
    handleReady(state);
    expect(state.isReloading).toBe(true);
    expect(state.isReady).toBe(false);
    handleRendererReady(state);
    expect(state.isReady).toBe(true);

    // Reload 3
    state.pendingMessages.push({ type: "render" });
    handleReady(state);
    expect(state.pendingMessages).toHaveLength(0);
  });

  it("does not set isReloading on initial load", () => {
    const state = initialState();
    handleReady(state);
    expect(state.isReloading).toBe(false);
  });

  it("preserves hasReceivedReady across renderer_ready", () => {
    const state = initialState();
    handleReady(state);
    handleRendererReady(state);
    // hasReceivedReady should still be true so next ready is detected as reload
    expect(state.hasReceivedReady).toBe(true);
  });

  it("resets bootstrapping on reload so renderer gets re-injected", () => {
    const state = initialState();
    handleReady(state);
    state.bootstrapping = true; // simulate in-progress bootstrap

    handleReady(state); // reload
    expect(state.bootstrapping).toBe(false);
  });
});

describe("send gating", () => {
  /**
   * The component gates message sending on isReady:
   * - If isReady, post directly to contentWindow
   * - If not isReady, queue in pendingMessages
   * During a reload, isReady is set to false, so messages queue up
   * until renderer_ready fires again.
   */
  function simulateSend(state: IframeState, message: unknown): "sent" | "queued" {
    if (state.isReady) {
      return "sent";
    }
    state.pendingMessages.push(message);
    return "queued";
  }

  it("sends directly when ready", () => {
    const state = initialState();
    handleReady(state);
    handleRendererReady(state);

    expect(simulateSend(state, { type: "render" })).toBe("sent");
  });

  it("queues messages before initial ready", () => {
    const state = initialState();
    expect(simulateSend(state, { type: "render" })).toBe("queued");
    expect(state.pendingMessages).toHaveLength(1);
  });

  it("queues messages during reload window", () => {
    const state = initialState();
    handleReady(state);
    handleRendererReady(state);

    handleReady(state); // reload — isReady becomes false
    expect(simulateSend(state, { type: "render" })).toBe("queued");
  });

  it("sends again after reload recovery", () => {
    const state = initialState();
    handleReady(state);
    handleRendererReady(state);

    handleReady(state); // reload
    handleRendererReady(state); // recovered

    expect(simulateSend(state, { type: "render" })).toBe("sent");
  });
});
