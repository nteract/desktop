import { afterEach, describe, expect, it } from "vite-plus/test";
import {
  DEFAULT_POOL_STATE,
  type PoolState,
  getPoolState,
  resetPoolState,
  setPoolState,
} from "../pool-state";

/**
 * pool-state.ts owns a module-level snapshot + subscriber Set. Tests
 * always call resetPoolState in afterEach so later tests see a clean
 * DEFAULT_POOL_STATE — otherwise a failure in one test bleeds through.
 */
describe("pool-state store", () => {
  afterEach(() => {
    resetPoolState();
  });

  const populated: PoolState = {
    uv: {
      available: 2,
      warming: 1,
      pool_size: 3,
      consecutive_failures: 0,
      retry_in_secs: 0,
    },
    conda: {
      available: 0,
      warming: 0,
      pool_size: 0,
      error: "uv not found",
      error_kind: "setup_failed",
      consecutive_failures: 2,
      retry_in_secs: 30,
      failed_package: "pandas",
    },
  };

  it("starts at DEFAULT_POOL_STATE with zeroed pools", () => {
    expect(getPoolState()).toEqual(DEFAULT_POOL_STATE);
    expect(getPoolState().uv.available).toBe(0);
    expect(getPoolState().conda.available).toBe(0);
    expect(getPoolState().uv.error).toBeUndefined();
  });

  it("setPoolState replaces the current snapshot", () => {
    setPoolState(populated);
    expect(getPoolState()).toBe(populated);
    expect(getPoolState().uv.available).toBe(2);
    expect(getPoolState().conda.error).toBe("uv not found");
    expect(getPoolState().conda.failed_package).toBe("pandas");
  });

  it("resetPoolState returns to DEFAULT_POOL_STATE", () => {
    setPoolState(populated);
    resetPoolState();
    expect(getPoolState()).toEqual(DEFAULT_POOL_STATE);
  });

  it("DEFAULT_POOL_STATE's nested pools are independent objects", () => {
    // If uv and conda aliased the same object (via module-level object
    // literal share), a daemon update that only touched `uv` would
    // silently mutate `conda` too — a real bug waiting to happen.
    expect(DEFAULT_POOL_STATE.uv).not.toBe(DEFAULT_POOL_STATE.conda);
  });

  it("setPoolState keeps the reference, not a defensive copy", () => {
    // useSyncExternalStore re-renders only when the snapshot reference
    // changes. If we deep-cloned, callers would over-render. If we kept
    // the old reference, callers would under-render. Pin: same-ref out.
    setPoolState(populated);
    expect(getPoolState()).toBe(populated);
  });

  it("successive setPoolState calls each replace the snapshot", () => {
    const first: PoolState = { ...populated };
    const second: PoolState = {
      ...populated,
      uv: { ...populated.uv, available: 7 },
    };
    setPoolState(first);
    expect(getPoolState().uv.available).toBe(2);
    setPoolState(second);
    expect(getPoolState().uv.available).toBe(7);
  });

  it("resetPoolState from pristine default is a no-op in terms of value", () => {
    // Daemon disconnect triggers resetPoolState even if we never had
    // state; it should behave identically whether we did or not.
    resetPoolState();
    expect(getPoolState()).toEqual(DEFAULT_POOL_STATE);
    resetPoolState();
    expect(getPoolState()).toEqual(DEFAULT_POOL_STATE);
  });

  it("operates safely with zero subscribers", () => {
    // Boot state has no React subscribers yet; the daemon may fire pool
    // updates immediately. setPoolState/resetPoolState must not throw
    // in that window.
    expect(() => setPoolState(populated)).not.toThrow();
    expect(() => resetPoolState()).not.toThrow();
  });

  it("setPoolState → resetPoolState → setPoolState round trip", () => {
    setPoolState(populated);
    resetPoolState();
    expect(getPoolState()).toEqual(DEFAULT_POOL_STATE);
    const fresh: PoolState = {
      uv: { ...DEFAULT_POOL_STATE.uv, available: 5 },
      conda: { ...DEFAULT_POOL_STATE.conda },
    };
    setPoolState(fresh);
    expect(getPoolState()).toBe(fresh);
    expect(getPoolState().uv.available).toBe(5);
  });
});
